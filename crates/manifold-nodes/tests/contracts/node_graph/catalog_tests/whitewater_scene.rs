//! The Whitewater chain on GPU FLIP's Dam Break, built by the testkit's
//! `whitewater_render_def`. The fuse test runs without a GPU. The `liquid_frame`
//! tests check the live frame against offline export, that a failed publication
//! still ends the frame with its outputs, and that a solid shrink keeps the mix
//! capacity.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use serde_json::json;

use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes;
use manifold_nodes_water::presets::gpu_flip::{WaterScene, render_def};

/// CPU-only regression: the probed scene must remain compilable after fusion.
#[test]
fn whitewater_scene_fuses_without_gpu() {
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use manifold_node_engine::{exec::execution_plan::compile, persistence::EffectGraphDefExt};

    let def = whitewater_render_def(WaterScene::dam_break(64));
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry.register(PROBE, || Box::new(Probe::new()));
    registry.register(COUNTS_PROBE, || Box::new(Probe::whitewater_counts()));
    let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
    let members: Vec<_> = report
        .nodes
        .iter()
        .filter(|node| node.fused)
        .map(|node| node.type_id.as_str())
        .collect();
    for expected in [
        "node.interpolate_particle_frames",
        "node.push_out_of_solid",
        "node.particles_to_copies",
    ] {
        assert!(
            members.contains(&expected),
            "{expected} must fuse; fused: {members:?}"
        );
    }

    let graph = def
        .clone()
        .into_graph(&registry, &Default::default())
        .expect("authored scene loads");
    let plan = compile(&graph).expect("probes must not escape the authored tick region");
    let state = graph
        .nodes()
        .find(|n| n.node_id.as_str() == "state")
        .expect("the liquid boundary")
        .id;
    assert!(
        plan.steps()
            .iter()
            .find(|s| s.node == state)
            .unwrap()
            .outputs
            .iter()
            .any(|(port, _)| *port == "whitewater_counts"),
        "the counts must stay bound for late capture"
    );
    let fused = fuse_generator_view(&def, &registry).expect("the whitewater scene must fuse");
    let graph = (*fused.def)
        .clone()
        .into_graph(&registry, &fused.mesh_rules)
        .expect("fused scene loads");
    let plan = compile(&graph).expect("probes must not escape the fused tick region");
    let state = graph
        .nodes()
        .find(|n| n.node_id.as_str() == "state")
        .expect("the liquid boundary survives fusion")
        .id;
    assert!(
        plan.steps()
            .iter()
            .find(|s| s.node == state)
            .unwrap()
            .outputs
            .iter()
            .any(|(port, _)| *port == "whitewater_counts"),
        "fusion must keep the counts bound for late capture"
    );
}

/// The Dam Break at 16 with the frame's presentation and the domain's
/// simulation time probed.
fn history_def() -> EffectGraphDef {
    let mut g = Appender::new(render_def(WaterScene::dam_break(16)));
    let frame = g.id("frame");
    for port in HISTORY_PROBES[..HISTORY_PROBES.len() - 1].iter() {
        g.probe(port, (frame, port));
    }
    let domain = g.id("domain");
    g.probe("simulation_time", (domain, "simulation_time"));
    g.finish()
}

/// [`history_def`] with the frame's `display_cursor` authored to `cursor`,
/// which the loader keeps.
fn authored_cursor_history_def(cursor: f32) -> EffectGraphDef {
    let mut g = Appender::new(history_def());
    let frame = g.id("frame");
    g.retain_wires(frame, |w| {
        !(w["toNode"] == frame && w["toPort"] == "display_cursor")
    });
    let exact = g.node_in_scope(
        "exact",
        "node.value",
        json!({"value": {"type": "Float", "value": cursor}}),
        frame,
    );
    g.wire((exact, "out"), frame, "display_cursor");
    g.finish()
}

/// The frame's presentation; the last is the domain's simulation time.
const HISTORY_PROBES: [&str; 9] = [
    "blend",
    "span",
    "count_a",
    "count_b",
    "identity_a",
    "identity_b",
    "presented_time",
    "publications_skipped",
    "simulation_time",
];
/// The probes that describe the selected pair.
const PAIR: std::ops::Range<usize> = 0..7;

/// A live
/// frame held until its publication retired shows what export shows at that
/// frame: the same endpoints and identities, A and B bytes, blend, span and
/// pixels. Export commit-waits each publication; live retires it on a later
/// frame, so after each frame live holds the clock until the presentation
/// matches, within eight frames.
#[test]
fn liquid_frame_live_held_frame_matches_offline() {
    const FRAMES: usize = 40;
    // Export with the cursor requested: liquid_frame's offline override must
    // still present exactly.
    let mut offline = Show::new(authored_cursor_history_def(1.0), (96, 54), false, &[]);
    offline.restart();
    let mut expected = Vec::new();
    for _ in 0..FRAMES {
        offline.frame(false);
        expected.push((
            offline.probes(HISTORY_PROBES),
            offline.provided_copy("frame", "particles_a"),
            offline.provided_copy("frame", "particles_b"),
            offline.readback(),
        ));
    }
    let live_step =
        manifold_water_rigid::physics::SimStep::live(manifold_core::Seconds(1.0 / 60.0))
            .with_preview_budget(std::time::Duration::from_secs(1));
    // Live runs exact: the cursor presents behind the request by design.
    let mut live = Show::new(authored_cursor_history_def(0.0), (96, 54), false, &[]);
    live.set_sim_step(live_step);
    live.restart();
    let mut holds = 0;
    for (k, (probes, particles_a, particles_b, pixels)) in expected.iter().enumerate() {
        live.frame(false);
        live.set_paused(true);
        let mut held = 0;
        while live.probes(HISTORY_PROBES)[PAIR] != probes[PAIR] {
            assert!(
                held < 8,
                "frame {k}: live never presented export's pair: live {:?}, export {probes:?}",
                live.probes(HISTORY_PROBES)
            );
            live.frame(false);
            held += 1;
        }
        holds += held;
        live.set_paused(false);
        assert_eq!(
            &live.provided_copy("frame", "particles_a"),
            particles_a,
            "frame {k}: the selected A differs"
        );
        assert_eq!(
            &live.provided_copy("frame", "particles_b"),
            particles_b,
            "frame {k}: the selected B differs"
        );
        assert!(
            live.readback() == *pixels,
            "frame {k}: pixels differ from export at the same presentation"
        );
        assert_eq!(
            live.probes(HISTORY_PROBES)[7],
            0.0,
            "no publication skipped"
        );
    }
    println!("live held {holds} frames over {FRAMES} until its publications retired");
}

/// A publication that fails to encode still ends the frame with the frame's
/// outputs: the selection happens before publishing, so the failing frame
/// shows exactly what the same frame shows without the failure, arrays and
/// scalars together, and names the error.
#[test]
fn liquid_frame_encode_failure_publishes_the_selected_outputs() {
    let live_step =
        manifold_water_rigid::physics::SimStep::live(manifold_core::Seconds(1.0 / 60.0))
            .with_preview_budget(std::time::Duration::from_secs(1));
    let [mut clean, mut failing] = [(), ()].map(|()| {
        let mut show = Show::new(history_def(), (96, 54), false, &[]);
        show.set_sim_step(live_step);
        show.restart();
        show
    });
    for frame in 0..30 {
        // Armed from frame 5 until a publication consumes it.
        let armed = frame >= 5;
        clean.frame(false);
        manifold_water_surface::primitives::liquid_frame::FAIL_NEXT_PUBLICATION.set(armed);
        failing.expect_node_error(armed);
        failing.frame(false);
        failing.expect_node_error(false);
        let inject =
            armed && !manifold_water_surface::primitives::liquid_frame::FAIL_NEXT_PUBLICATION.get();
        manifold_water_surface::primitives::liquid_frame::FAIL_NEXT_PUBLICATION.set(false);
        if inject {
            assert!(
                failing.last_status().starts_with("Failed"),
                "frame {frame}: the failure is reported: {}",
                failing.last_status()
            );
            assert_eq!(
                failing.probes(HISTORY_PROBES)[PAIR],
                clean.probes(HISTORY_PROBES)[PAIR],
                "frame {frame}: scalars follow the selection"
            );
            for port in ["particles_a", "particles_b"] {
                assert_eq!(
                    failing.provided_copy("frame", port),
                    clean.provided_copy("frame", port),
                    "frame {frame}: {port}"
                );
            }
            // The failed endpoint is never retried, so the two runs part.
            return;
        }
    }
    panic!("no publication consumed the injected failure");
}

/// A smaller lattice keeps the solid's storage, wired or as walls: the
/// solid mix's output capacity is planned once and never shrinks.
#[test]
fn liquid_frame_solid_shrink_keeps_mix_capacity() {
    for wired in [true, false] {
        let mut g = Appender::new(render_def(WaterScene::dam_break(32)));
        let frame = g.id("frame");
        if !wired {
            g.retain_wires(frame, |w| !(w["toNode"] == frame && w["toPort"] == "solid"));
        }
        let def = g.finish();
        let spec = def
            .preset_metadata
            .as_ref()
            .and_then(|cards| cards.params.iter().find(|card| card.id == "resolution"))
            .expect("the Resolution card")
            .clone();
        let mut show = Show::new(def, (96, 54), false, &[]);
        show.restart();
        for n in [32u32, 16] {
            let mut card = Param::bundled(spec.clone());
            card.value = n as f32;
            card.base = n as f32;
            show.set_cards(ParamManifest::from_params(vec![card]));
            for _ in 0..4 {
                show.frame(false);
            }
            assert!(
                show.errors().is_empty(),
                "solid wired {wired}, Resolution {n}: {:?}",
                show.errors()
            );
        }
    }
}

use manifold_nodes_water::testkit::whitewater_scene::*;
