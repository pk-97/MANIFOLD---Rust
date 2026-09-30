use super::*;

#[test]
fn scene_impulse_source_requires_rebuild_after_output_roots_change() {
    let def = fixture();
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let mut binding = prepare(&runtime, &def, &["part_a"]);
    let mut hit = binding.new_capture();
    let field = runtime
        .graph
        .instance_by_node_id(&NodeId::new("field"))
        .unwrap();
    runtime.graph.add_external_output(field, "out").unwrap();
    assert!(
        runtime
            .capture_scene_impulse_at_source(&mut binding, &mut hit, time(0.1), 0,)
            .unwrap_err()
            .contains("outputs changed")
    );
    assert_eq!(
        runtime.last_physics_frame_time.unwrap().seconds,
        Seconds::ZERO
    );
    runtime.execute_frame(time(0.1));
    assert!(runtime.awaiting_forced_outputs_rebuild());
    assert!(
        runtime
            .prepare_scene_impulse(
                &def,
                &reference("scene"),
                &SceneTargetSelection::AllObjects,
                &NodeId::new("field"),
                "out",
            )
            .is_err(),
        "marking a plan stale is not a completed rebuild"
    );
}

#[test]
fn scene_impulse_source_captures_fluid_clock_and_waits_for_changed_setup() {
    let mut def = fixture();
    def.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id":14,"nodeId":"fluid","typeId":manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID,"params":{
                "resolution":{"type":"Int","value":8},
                "fill_height":{"type":"Float","value":0.0},
                "emission":{"type":"Float","value":0.0}
            }
        }))
        .unwrap(),
    );
    def.wires
        .retain(|wire| !(wire.to_node == 9 && wire.to_port == "transform"));
    def.wires.push(
        serde_json::from_value(serde_json::json!({
            "fromNode":14,"fromPort":"vertices","toNode":9,"toPort":"vertices"
        }))
        .unwrap(),
    );
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(2.0));
    let mut binding = prepare(&runtime, &def, &["part_b"]);
    let mut hit = binding.new_capture();
    // A connected world and domain consume one shared simulation clock.
    edit(&mut runtime, "world", "speed", 2.0);
    edit(&mut runtime, "fluid", "speed", 2.0);
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(2.05), 0)
        .unwrap();
    assert!((hit.stamps[0].time.0 - 0.05).abs() < 1e-12);
    runtime.deliver_scene_impulse(&mut hit).unwrap();
    hit.clear();
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(2.10), 1)
        .unwrap();
    assert!((hit.stamps[0].time.0 - 0.15).abs() < 1e-12);
    let epoch = hit.stamps[0].epoch;
    runtime.deliver_scene_impulse(&mut hit).unwrap();
    let mut receipts = 0;
    runtime.drain_scene_impulses(|_, _| receipts += 1);
    assert_eq!(receipts, 0, "capturing cannot run the native fluid worker");
    runtime.execute_frame(time(2.10));
    let mut receipts = Vec::new();
    runtime.drain_scene_impulses(|_, receipt| receipts.push(receipt));
    assert_eq!(
        receipts.len(),
        1,
        "the 0.15 boundary hit belongs to the next tick"
    );
    assert_eq!(receipts[0].source.sequence, 0);
    hit.clear();
    edit(&mut runtime, "fluid", "fill_height", 0.1);
    assert!(
        runtime
            .capture_scene_impulse_at_source(&mut binding, &mut hit, time(2.10), 2,)
            .is_err(),
        "setup changes require a full scene evaluation"
    );
    assert!(hit.source_time().is_none());
    // Keep the verification domain empty; the setup still changes epoch.
    edit(&mut runtime, "fluid", "fill_height", 0.0);
    edit(&mut runtime, "fluid", "domain_size", 3.0);
    runtime.execute_frame(time(2.10));
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(2.10), 2)
        .unwrap();
    assert_ne!(hit.stamps[0].epoch, epoch);
    assert_eq!(hit.stamps[0].time, Seconds::ZERO);
}

#[test]
fn scene_impulse_source_maps_speed_edits_and_pause_without_stepping() {
    let def = fixture();
    let mut runtime = runtime(&def);
    let mut binding = prepare(&runtime, &def, &["part_a"]);
    let mut hit = binding.new_capture();
    assert!(
        runtime
            .capture_scene_impulse_at_source(&mut binding, &mut hit, time(10.0), 0,)
            .unwrap_err()
            .contains("render the scene")
    );
    runtime.execute_frame(time(10.0));
    edit(&mut runtime, "world", "speed", 2.0);
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(10.125), 0)
        .unwrap();
    assert_eq!(
        hit.stamps[0].time,
        Seconds(0.125),
        "close old speed before applying edit"
    );
    runtime.deliver_scene_impulse(&mut hit).unwrap();
    hit.clear();
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(10.25), 1)
        .unwrap();
    assert_eq!(hit.stamps[0].time, Seconds(0.375));
    runtime.deliver_scene_impulse(&mut hit).unwrap();
    hit.clear();
    edit(&mut runtime, "world", "speed", 0.0);
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(10.25), 2)
        .unwrap();
    assert_eq!(hit.stamps[0].time, Seconds(0.375));
    runtime.deliver_scene_impulse(&mut hit).unwrap();
    hit.clear();
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(10.5), 3)
        .unwrap();
    assert_eq!(hit.stamps[0].time, Seconds(0.375));
    assert_eq!(
        POSITIONS.get(),
        [0.0, 5.0],
        "source capture never runs a native tick"
    );
    runtime.deliver_scene_impulse(&mut hit).unwrap();
    hit.clear();
    // Resume at the same transport time: a subsequent frame must drain each
    // old interval once, retaining all four discrete hits.
    edit(&mut runtime, "world", "speed", 1.0);
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(10.5), 4)
        .unwrap();
    hit.clear();
    runtime.execute_frame(time(10.6));
    let mut receipts = Vec::new();
    runtime.drain_scene_impulses(|_, receipt| receipts.push(receipt));
    assert_eq!(receipts.len(), 4);
    assert_eq!(
        receipts.iter().map(|r| r.applied.tick).collect::<Vec<_>>(),
        [7, 22, 22, 22]
    );
    assert_eq!(
        runtime
            .graph
            .get_node(
                runtime
                    .graph
                    .instance_by_node_id(&NodeId::new("world"))
                    .unwrap()
            )
            .unwrap()
            .node
            .physics_impulse_stamp(Seconds(10.6), 5)
            .unwrap()
            .time
            .0,
        0.375 + (10.6 - 10.5)
    );
}

#[test]
fn scene_impulse_source_rejects_old_or_pending_capture_without_moving_anchor() {
    let def = fixture();
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(1.0));
    let mut binding = prepare(&runtime, &def, &["part_a"]);
    let mut hit = binding.new_capture();
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(1.1), 0)
        .unwrap();
    assert!(
        runtime
            .capture_scene_impulse_at_source(&mut binding, &mut hit, time(1.2), 1,)
            .unwrap_err()
            .contains("acknowledge")
    );
    assert_eq!(
        runtime.last_physics_frame_time.unwrap().seconds,
        Seconds(1.1)
    );
    hit.clear();
    assert!(
        runtime
            .capture_scene_impulse_at_source(&mut binding, &mut hit, time(1.05), 1,)
            .unwrap_err()
            .contains("precedes")
    );
    assert_eq!(
        runtime.last_physics_frame_time.unwrap().seconds,
        Seconds(1.1)
    );
    assert!(
        runtime
            .capture_scene_impulse_at_source(&mut binding, &mut hit, time(f64::NAN), 1,)
            .unwrap_err()
            .contains("finite")
    );
    edit(&mut runtime, "world", "reset", 1.0);
    assert!(
        runtime
            .capture_scene_impulse_at_source(&mut binding, &mut hit, time(1.1), 1,)
            .is_err(),
        "a withheld reset must not admit an event into the old epoch"
    );
    assert!(hit.source_time().is_none());
    runtime.execute_frame(time(1.1));
    runtime
        .capture_scene_impulse_at_source(&mut binding, &mut hit, time(1.1), 1)
        .unwrap();
    assert_eq!(hit.stamps[0].time, Seconds::ZERO);
}

#[test]
fn scene_impulse_source_ticks_match_across_frame_rates_and_display_stall() {
    let mut def = fixture();
    def.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id":30,"nodeId":"speed_lfo","typeId":"node.lfo","params":{
                "rate_mode":{"type":"Enum","value":1},
                "angular_rate":{"type":"Float","value":5.0},
                "min":{"type":"Float","value":0.5},
                "max":{"type":"Float","value":1.5}
            }
        }))
        .unwrap(),
    );
    def.wires.push(
        serde_json::from_value(serde_json::json!({
            "fromNode":30,"fromPort":"out","toNode":6,"toPort":"speed"
        }))
        .unwrap(),
    );
    let run = |fps: Option<u32>| {
        let mut runtime = runtime(&def);
        runtime.execute_frame(time(0.0));
        let mut binding = prepare(&runtime, &def, &["part_a"]);
        let mut hit = binding.new_capture();
        let events = [0.137, 0.291];
        let mut boundaries: Vec<_> = events.iter().copied().map(|t| (t, true)).collect();
        if let Some(fps) = fps {
            boundaries.extend((1..=fps / 2).map(|frame| (frame as f64 / fps as f64, false)));
        } else {
            boundaries.push((0.5, false));
        }
        boundaries.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut sequence = 0;
        let mut stamps = Vec::new();
        for (seconds, event) in boundaries {
            if event {
                edit(&mut runtime, "field", "x", 3.0 + sequence as f32);
                runtime
                    .capture_scene_impulse_at_source(
                        &mut binding,
                        &mut hit,
                        time(seconds),
                        sequence,
                    )
                    .unwrap();
                stamps.push(hit.stamps[0]);
                runtime.deliver_scene_impulse(&mut hit).unwrap();
                hit.clear();
                sequence += 1;
            } else {
                runtime.execute_frame(time(seconds));
            }
        }
        let mut receipts = Vec::new();
        runtime.drain_scene_impulses(|_, event| receipts.push(event));
        assert_eq!(receipts.len(), 2);
        (stamps, receipts, POSITIONS.get())
    };
    let reference = run(Some(60));
    for fps in [Some(24), Some(30), None] {
        let actual = run(fps);
        for (a, b) in actual.0.iter().zip(&reference.0) {
            assert_eq!(a.sequence, b.sequence);
            assert!((a.time.0 - b.time.0).abs() < 1e-9, "{fps:?}: {a:?} / {b:?}");
        }
        for (a, b) in actual.1.iter().zip(&reference.1) {
            assert_eq!(a.applied, b.applied);
            assert_eq!(a.value, b.value);
        }
        assert!(
            (actual.2[0] - reference.2[0]).abs() < 1e-5,
            "{fps:?}: {:?} / {:?}",
            actual.2,
            reference.2
        );
    }
}
