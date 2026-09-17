//! Exercise the same rendered control through both complete panel input paths.
use super::*;
use crate::input::Modifiers;
use crate::panels::param_card::ParamCardPanel;
use crate::panels::param_slider_shared::AudioRowState;
use crate::panels::{AudioShapeParam, TrimKind};

#[derive(Clone, Copy, Debug)]
enum Control {
    Parameter,
    Trim(TrimKind, bool),
    EnvelopeTarget,
    EnvelopeDecay,
    AudioShape(usize),
    AudioStep,
}

fn setup(control: Control) -> (ScenePanel, UITree, ParamCardPanel, UITree) {
    let (vm, mut surface) = tests::world_transform_vm();
    surface.supports_envelopes = true;
    let row = &mut surface.rows[0];
    match control {
        Control::Trim(TrimKind::Driver, _) => {
            row.modulation.driver_active = true;
            row.modulation.trim_min = 0.2;
            row.modulation.trim_max = 0.8;
        }
        Control::Trim(TrimKind::Ableton, _) => row.mapping.ableton_range = Some((0.2, 0.8)),
        Control::Trim(TrimKind::Audio, _) | Control::AudioShape(_) | Control::AudioStep => {
            row.audio = AudioRowState {
                active: true,
                range_min: 0.2,
                range_max: 0.8,
                action_idx: if matches!(control, Control::AudioStep) {
                    1
                } else {
                    0
                },
                ..Default::default()
            };
        }
        Control::EnvelopeTarget | Control::EnvelopeDecay => {
            row.modulation.envelope_active = true;
            row.modulation.target_norm = 0.8;
        }
        Control::Parameter => {}
    }
    let mut card = ParamCardPanel::new();
    card.configure(&surface);
    let mut card_tree = UITree::new();
    card.build(&mut card_tree, Rect::new(0.0, 0.0, 500.0, 1400.0));
    let mut scene = ScenePanel::new();
    scene.open();
    scene.configure(SceneSetupState::Live(Box::new(vm)));
    scene.configure_params(Some(surface));
    scene
        .selection
        .insert(LayerId::new("layer-1"), SceneSelection::World);
    let mut scene_tree = UITree::new();
    scene.build_docked(&mut scene_tree, Rect::new(0.0, 0.0, 500.0, 1600.0));
    (scene, scene_tree, card, card_tree)
}

fn scene_control(scene: &ScenePanel, control: Control) -> (NodeId, NodeId) {
    let host = &scene.properties_card.row_host;
    let main = host.slider_ids[0].unwrap().track;
    match control {
        Control::Parameter => (main, main),
        Control::Trim(kind, is_min) => {
            let trim = match kind {
                TrimKind::Driver => host.trim_ids[0],
                TrimKind::Ableton => host.ableton_trim_ids[0],
                TrimKind::Audio => host.audio_trim_ids[0],
            }
            .expect("a rendered trim must be registered");
            (
                if is_min {
                    trim.min_bar_id
                } else {
                    trim.max_bar_id
                },
                main,
            )
        }
        Control::EnvelopeTarget => (host.target_ids[0].as_ref().unwrap().target_bar_id, main),
        Control::EnvelopeDecay => {
            let track = host.envelope_config_ids[0]
                .as_ref()
                .unwrap()
                .decay_slider
                .as_ref()
                .unwrap()
                .track;
            (track, track)
        }
        Control::AudioShape(index) => {
            let track = host.audio_configs[0].as_ref().unwrap().0.sliders[index].track;
            (track, track)
        }
        Control::AudioStep => {
            let track = host.audio_configs[0].as_ref().unwrap().0.sliders[3].track;
            (track, track)
        }
    }
}

// Match the same shared builder's affordance in the public inspector catalog.
// This verifies registration in both hosts without reaching into card internals.
fn card_control(
    scene: &ScenePanel,
    scene_tree: &UITree,
    scene_node: NodeId,
    card: &ParamCardPanel,
    tree: &UITree,
) -> NodeId {
    let index = &scene.properties_card.row_host.row_index;
    let expected = index
        .get(scene_tree.widget_of(scene_node))
        .expect("scene control registered");
    let ordinal = (0..scene_tree.count())
        .map(|i| scene_tree.id_at(i))
        .filter(|id| index.get(scene_tree.widget_of(*id)) == Some(expected))
        .position(|id| id == scene_node)
        .unwrap();
    let catalog = card.catalog(tree);
    let widget = catalog
        .affordances
        .iter()
        .filter(|entry| entry.row_id == "translate_x" && entry.role == expected.1)
        .nth(ordinal)
        .expect("inspector must register the same control")
        .widget;
    (0..tree.count())
        .map(|i| tree.id_at(i))
        .find(|id| tree.widget_of(*id).raw() == widget)
        .unwrap()
}

fn point(tree: &UITree, id: NodeId, norm: f32) -> Vec2 {
    let r = tree.get_bounds(id);
    Vec2::new(r.x + r.width * norm, r.y + r.height * 0.5)
}

fn expected_address(control: Control, target: GraphParamTarget) -> ValueRef {
    let id = "translate_x".into();
    match control {
        Control::Parameter => ValueRef::Param(target, id),
        Control::Trim(kind, _) => ValueRef::Trim(kind, target, id),
        Control::EnvelopeTarget => ValueRef::EnvelopeTarget(target, id),
        Control::EnvelopeDecay => ValueRef::EnvDecay(target, id),
        Control::AudioShape(i) => ValueRef::AudioModShape(
            target,
            id,
            [
                AudioShapeParam::Sensitivity,
                AudioShapeParam::Attack,
                AudioShapeParam::Release,
            ][i],
        ),
        Control::AudioStep => ValueRef::AudioModStepAmount(target, id),
    }
}

fn phases(actions: Vec<PanelAction>, expected: &ValueRef) -> Vec<ScrubPhase> {
    actions
        .into_iter()
        .map(|action| match action {
            PanelAction::Scrub(address, phase) => {
                assert_eq!(&address, expected);
                phase
            }
            other => panic!("expected scrub, got {other:?}"),
        })
        .collect()
}

fn assert_same_phases(scene: &[ScrubPhase], card: &[ScrubPhase]) {
    assert_eq!(scene.len(), card.len());
    for (a, b) in scene.iter().zip(card) {
        match (a, b) {
            (ScrubPhase::Move(ScrubValue::Scalar(a)), ScrubPhase::Move(ScrubValue::Scalar(b))) => {
                assert!((a - b).abs() < 0.001, "{a} != {b}")
            }
            (
                ScrubPhase::Move(ScrubValue::Range(a, b)),
                ScrubPhase::Move(ScrubValue::Range(c, d)),
            ) => assert!((a - c).abs() < 0.001 && (b - d).abs() < 0.001),
            _ => assert_eq!(a, b),
        }
    }
}

#[test]
fn every_scene_row_drag_matches_the_inspector_contract() {
    let controls = [
        Control::Parameter,
        Control::Trim(TrimKind::Driver, true),
        Control::Trim(TrimKind::Driver, false),
        Control::Trim(TrimKind::Ableton, true),
        Control::Trim(TrimKind::Ableton, false),
        Control::Trim(TrimKind::Audio, true),
        Control::Trim(TrimKind::Audio, false),
        Control::EnvelopeTarget,
        Control::EnvelopeDecay,
        Control::AudioShape(0),
        Control::AudioShape(1),
        Control::AudioShape(2),
        Control::AudioStep,
    ];
    for control in controls {
        let (mut scene, mut scene_tree, mut card, mut card_tree) = setup(control);
        let (scene_node, scene_track) = scene_control(&scene, control);
        let card_node = card_control(&scene, &scene_tree, scene_node, &card, &card_tree);
        let card_track = card_control(&scene, &scene_tree, scene_track, &card, &card_tree);
        let scene_address = expected_address(
            control,
            GraphParamTarget::GeneratorOf(LayerId::new("layer-1")),
        );
        let card_address = expected_address(control, GraphParamTarget::Generator);
        let scene_pos = point(&scene_tree, scene_node, 0.5);
        let card_pos = point(&card_tree, card_node, 0.5);
        assert_eq!(
            scene_tree.hit_test(scene_pos),
            Some(scene_node),
            "scene {control:?} must be hittable"
        );
        assert_eq!(
            card_tree.hit_test(card_pos),
            Some(card_node),
            "card {control:?} must be hittable"
        );
        let (consumed, actions) = scene.handle_event(
            &UIEvent::PointerDown {
                node_id: scene_node,
                pos: scene_pos,
                modifiers: Modifiers::NONE,
            },
            &mut scene_tree,
        );
        assert!(consumed);
        let scene_phases = phases(actions, &scene_address);
        let card_phases = phases(
            card.handle_pointer_down(card_node, card_pos, &card_tree),
            &card_address,
        );
        assert!(
            matches!(scene_phases.first(), Some(ScrubPhase::Begin)),
            "{control:?}"
        );
        assert_same_phases(&scene_phases, &card_phases);
        for fine in [false, true] {
            let scene_pos = point(&scene_tree, scene_track, 0.7);
            let card_pos = point(&card_tree, card_track, 0.7);
            let (consumed, actions) = scene.handle_event(
                &UIEvent::Drag {
                    node_id: None,
                    pos: scene_pos,
                    delta: Vec2::ZERO,
                    modifiers: Modifiers {
                        shift: fine,
                        ..Modifiers::NONE
                    },
                },
                &mut scene_tree,
            );
            assert!(consumed);
            let scene_phases = phases(actions, &scene_address);
            let card_phases = phases(
                card.handle_drag(card_pos, &mut card_tree, fine),
                &card_address,
            );
            assert!(
                matches!(scene_phases.as_slice(), [ScrubPhase::Move(_)]),
                "{control:?}"
            );
            assert_same_phases(&scene_phases, &card_phases);
            if matches!(control, Control::EnvelopeTarget) {
                let expected = crate::panels::param_slider_shared::target_bar_rect(
                    scene_tree.get_bounds(scene_track),
                    0.7,
                );
                let actual = scene_tree.get_bounds(scene_node);
                assert!((actual.x - expected.x).abs() < 0.001 && actual.y == expected.y,
                    "target handle must move immediately without a rebuild");
            }
        }
        let (_, actions) = scene.handle_event(
            &UIEvent::DragEnd {
                node_id: None,
                pos: Vec2::ZERO,
            },
            &mut scene_tree,
        );
        let scene_phases = phases(actions, &scene_address);
        let card_phases = phases(card.handle_drag_end(&mut card_tree), &card_address);
        assert_eq!(scene_phases, [ScrubPhase::Commit]);
        assert_same_phases(&scene_phases, &card_phases);
        let (_, actions) = scene.handle_event(
            &UIEvent::PointerUp {
                node_id: None,
                pos: Vec2::ZERO,
            },
            &mut scene_tree,
        );
        assert!(actions.is_empty());
        assert!(card.handle_drag_end(&mut card_tree).is_empty());
    }
}
