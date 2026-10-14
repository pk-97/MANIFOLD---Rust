//! The content half of the count gate (docs/UI_INVALIDATION_DESIGN.md D10, P1).
//!
//! Drives the real `ContentThread::handle_command` and the real
//! `tick_frame` on a headless content thread, with commands the real
//! `ui_bridge::dispatch` produces, and counts the content-thread work each
//! gesture costs. Ceilings live in `scripts/ui-flows/work_ceilings.json` under
//! `content_reaction_budget`; a kind with no ceiling has a ceiling of zero.
//! P1 records today's counts; later phases re-record them downward.
//!
//! Re-record (the only writer of this section, D11):
//! `MANIFOLD_RECORD_WORK=1 cargo nextest run -p manifold-app content_reaction_budget`.
//!
//! `tempo_ramp_matches_today` is D24's oracle: beat and time per tick through a
//! tempo-recording session, compared to values recorded from today's code
//! (`testdata/tempo_ramp_today.json`, same env var to re-record).

use crossbeam_channel::{Receiver, Sender};
use manifold_core::change::{ChangeSet, Part};
use manifold_core::effects::{AutomationLane, AutomationPoint, ParamEnvelope, ParameterDriver, SegmentShape};
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform, LayerType, TempoPointSource};
use manifold_core::work_count::{thread_work_snapshot, WorkKind, WorkSnapshot};
use manifold_core::{Beats, Bpm, LayerId, Seconds};
use manifold_ui::panels::{
    AudioShapeParam, BandDivider, GraphParamTarget, PanelAction, ScrubPhase, ScrubValue, TrimKind,
    UiRelightField, ValueRef,
};

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::content_thread::ContentThread;
use crate::ui_root::UIRoot;

const RECORD_ENV: &str = "MANIFOLD_RECORD_WORK";
const CEILINGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/ui-flows/work_ceilings.json");
const TEMPO_ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata/tempo_ramp_today.json");

fn recording() -> bool {
    std::env::var(RECORD_ENV).is_ok_and(|v| v == "1")
}

fn effect(type_id: &str) -> manifold_core::effects::PresetInstance {
    let mut e = manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::from_string(type_id.to_string()));
    e.init_defaults();
    e
}

fn param_ids(fx: &manifold_core::effects::PresetInstance) -> Vec<String> {
    manifold_core::preset_definition_registry::try_get(fx.effect_type())
        .map(|def| def.param_defs.iter().map(|pd| pd.spec.id.to_string()).collect())
        .unwrap_or_default()
}

/// One video layer carrying every live-drag surface section 1 names: an LFO
/// driver, an Ableton mapping, an audio mod, an envelope, an automation lane,
/// a layer clip trigger and audio gain; one send; a generator layer.
fn budget_project() -> Project {
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource, ParameterAudioMod};
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;

    let kick = AudioSend::new("Kick");
    let kick_id = kick.id.clone();

    let mut glow = Layer::new("GLOW".into(), LayerType::Video, 0);
    glow.layer_id = LayerId::new("glow");
    glow.clips.push(TimelineClip::new_video("loop.mov".into(), Beats(0.0), Beats(16.0), Seconds::ZERO));

    let mut lfo_fx = effect("Mirror");
    let lfo_params = param_ids(&lfo_fx);
    let pid = |s: &String| -> manifold_core::effects::ParamId { std::borrow::Cow::Owned(s.clone()) };
    lfo_fx.drivers = Some(vec![ParameterDriver::new(pid(&lfo_params[0]), BeatDivision::Quarter, DriverWaveform::Sine)]);
    if let Some(p) = lfo_params.get(1) {
        lfo_fx.ableton_mappings = Some(vec![manifold_core::ableton_mapping::AbletonParamMapping {
            param_id: p.clone().into(),
            address: manifold_core::ableton_mapping::AbletonMacroAddress {
                track_id: 0,
                device_id: 0,
                param_id: 1,
                device_identity: manifold_core::ableton_mapping::AbletonDeviceIdentity { device_class_name: "AudioEffectGroupDevice".into() },
                track_name: "1-Audio".into(),
                device_name: "Rack".into(),
                macro_name: "Macro 1".into(),
            },
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: manifold_core::ableton_mapping::AbletonMappingStatus::Active,
        }]);
        lfo_fx.automation_lanes = Some(vec![AutomationLane {
            param_id: p.clone().into(),
            enabled: true,
            points: vec![
                AutomationPoint { beat: Beats(0.0), value: 0.1, shape: SegmentShape::Linear },
                AutomationPoint { beat: Beats(8.0), value: 0.9, shape: SegmentShape::Linear },
            ],
        }]);
    }

    let mut mod_fx = effect("Bloom");
    let mod_params = param_ids(&mod_fx);
    mod_fx.audio_mods = Some(vec![ParameterAudioMod::new(
        pid(&mod_params[0]),
        kick_id.clone(),
        AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
    )]);

    let mut env_fx = effect("Mirror");
    let env_params = param_ids(&env_fx);
    env_fx.envelopes = Some(vec![ParamEnvelope::new(pid(&env_params[0]))]);

    glow.effects = Some(vec![lfo_fx, mod_fx, env_fx]);
    let mut trigger = manifold_core::audio_trigger::LayerClipTrigger::new(AudioModSource {
        send_id: kick_id,
        feature: AudioFeature::new(AudioFeatureKind::Kick, AudioBand::Low),
    });
    trigger.enabled = true;
    glow.clip_triggers.push(trigger);

    let mut plasma = Layer::new_generator("PLASMA".into(), manifold_core::PresetTypeId::PLASMA, 1);
    plasma.layer_id = LayerId::new("plasma");
    plasma.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(16.0)));

    let mut project = Project::default();
    project.audio_setup.sends.push(kick);
    project.timeline.layers = vec![glow, plasma];
    project
}

/// The UI half: a real `UIRoot` and dispatch context against the UI's own
/// project copy, as the live App holds them.
struct UiSide {
    ui: UIRoot,
    project: Project,
    selection: manifold_ui::UIState,
    active: Option<LayerId>,
    prefs: crate::user_prefs::UserPrefs,
    scrub: crate::ui_bridge::ScrubState,
    state: ContentState,
    tx: Sender<ContentCommand>,
    rx: Receiver<ContentCommand>,
}

impl UiSide {
    fn new(project: Project) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut selection = manifold_ui::UIState::default();
        selection.select_layer(LayerId::new("glow"));
        Self {
            ui: UIRoot::new(),
            project,
            selection,
            active: Some(LayerId::new("glow")),
            prefs: crate::user_prefs::UserPrefs::in_memory(),
            scrub: crate::ui_bridge::ScrubState::default(),
            state: ContentState::default(),
            tx,
            rx,
        }
    }

    /// Dispatch through the real bridge; return what it sent the content thread.
    fn dispatch(&mut self, action: PanelAction) -> Vec<ContentCommand> {
        let mut stamps = crate::frame_plan::UiStamps::default();
        let mut ctx = crate::ui_bridge::DispatchCtx {
            project: crate::ui_bridge::ProjectRef::new(&mut self.project, &mut stamps, &self.tx),
            content_tx: &self.tx,
            content_state: &self.state,
            ui: &mut self.ui,
            selection: &mut self.selection,
            active_layer: &mut self.active,
            user_prefs: &mut self.prefs,
            editor_target: None,
            scrub: &mut self.scrub,
            dialogs: Default::default(),
        };
        crate::ui_bridge::dispatch(&action, &mut ctx);
        self.rx.try_iter().collect()
    }
}

struct Bench {
    content: ContentThread,
    state_tx: Sender<ContentState>,
    state_rx: Receiver<ContentState>,
    ui: UiSide,
}

impl Bench {
    fn new() -> Self {
        let project = budget_project();
        let mut content = crate::headless_harness::headless_content_thread(project.clone(), 64, 64);
        content.timer.set_frame_clocked(true);
        let (state_tx, state_rx) = crossbeam_channel::unbounded();
        let mut bench = Self { content, state_tx, state_rx, ui: UiSide::new(project) };
        bench.ticks(5);
        bench
    }

    /// Real content ticks; the UI drains states each frame, so this does too
    /// (an undrained channel trips `send_state`'s backlog cut-off).
    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.content.tick_frame(&self.state_tx);
            while self.state_rx.try_recv().is_ok() {}
        }
    }

    fn send(&mut self, commands: Vec<ContentCommand>) {
        for command in commands {
            self.content.handle_command(command);
        }
    }

    /// Settle, then count the content-thread work `gesture` costs.
    fn measure(&mut self, gesture: impl FnOnce(&mut Self)) -> WorkSnapshot {
        self.ticks(3);
        let before = thread_work_snapshot();
        gesture(self);
        thread_work_snapshot().since(&before)
    }

    /// One live-drag move as the live App sends it: Begin and one Move
    /// through dispatch, then the next content tick. The Commit follows
    /// outside the measured window.
    fn scrub_tick(&mut self, address: ValueRef, value: ScrubValue) -> WorkSnapshot {
        let begin = self.ui.dispatch(PanelAction::Scrub(address.clone(), ScrubPhase::Begin));
        self.send(begin);
        let counts = self.measure(|b| {
            let moved = b.ui.dispatch(PanelAction::Scrub(address.clone(), ScrubPhase::Move(value)));
            b.send(moved);
            b.ticks(1);
        });
        let commit = self.ui.dispatch(PanelAction::Scrub(address, ScrubPhase::Commit));
        self.send(commit);
        counts
    }
}

#[test]
fn content_reaction_budget() {
    let mut bench = Bench::new();
    let fx = |i| GraphParamTarget::Effect(i);
    let lfo_params = param_ids(&bench.ui.project.timeline.layers[0].effects.as_ref().unwrap()[0]);
    let mod_params = param_ids(&bench.ui.project.timeline.layers[0].effects.as_ref().unwrap()[1]);
    let p = |s: &str| std::borrow::Cow::Owned(s.to_string());
    let send_id = bench.ui.project.audio_setup.sends[0].id.clone();
    let glow = LayerId::new("glow");

    let mut cases: Vec<(&str, WorkSnapshot)> = vec![("idle_ticks_x10", bench.measure(|b| b.ticks(10)))];
    cases.push(("led_tick", bench.scrub_tick(ValueRef::LedBrightness, ScrubValue::Scalar(0.4))));
    cases.push((
        "ableton_trim_tick",
        bench.scrub_tick(ValueRef::Trim(TrimKind::Ableton, fx(0), p(&lfo_params[1])), ScrubValue::Range(0.1, 0.8)),
    ));
    cases.push((
        "param_slider_tick",
        bench.scrub_tick(ValueRef::Param(fx(1), p(&mod_params[0])), ScrubValue::Scalar(0.6)),
    ));
    // The Commit of a slider gesture: the one undoable Execute.
    {
        let address = ValueRef::Param(fx(1), p(&mod_params[0]));
        let begin = bench.ui.dispatch(PanelAction::Scrub(address.clone(), ScrubPhase::Begin));
        bench.send(begin);
        let moved = bench.ui.dispatch(PanelAction::Scrub(address.clone(), ScrubPhase::Move(ScrubValue::Scalar(0.3))));
        bench.send(moved);
        let commit = bench.ui.dispatch(PanelAction::Scrub(address, ScrubPhase::Commit));
        assert!(
            commit.iter().any(|c| matches!(c, ContentCommand::Execute(_))),
            "a slider commit sends one Execute"
        );
        cases.push(("param_execute", bench.measure(|b| {
            b.send(commit);
            b.ticks(1);
        })));
    }
    cases.push(("undo", bench.measure(|b| {
        b.send(vec![ContentCommand::Undo]);
        b.ticks(1);
    })));
    {
        // The overlay host's clip-move commit (`commit_command_batch`).
        let clip = bench.ui.project.timeline.layers[0].clips[0].id.clone();
        let mv = manifold_editing::commands::clip::MoveClipCommand::new(clip, Beats(0.0), Beats(2.0), glow.clone(), glow.clone());
        cases.push(("clip_move_execute", bench.measure(|b| {
            b.send(vec![ContentCommand::ExecuteBatch(vec![Box::new(mv)], "Move clip".into())]);
            b.ticks(1);
        })));
    }
    cases.push((
        "driver_trim_tick",
        bench.scrub_tick(ValueRef::Trim(TrimKind::Driver, fx(0), p(&lfo_params[0])), ScrubValue::Range(0.2, 0.7)),
    ));
    let env_params = param_ids(&bench.ui.project.timeline.layers[0].effects.as_ref().unwrap()[2]);
    cases.push((
        "envelope_tick",
        bench.scrub_tick(ValueRef::EnvelopeTarget(fx(2), p(&env_params[0])), ScrubValue::Scalar(0.5)),
    ));
    cases.push((
        "audio_mod_tick",
        bench.scrub_tick(
            ValueRef::AudioModShape(fx(1), p(&mod_params[0]), AudioShapeParam::Sensitivity),
            ScrubValue::Scalar(0.7),
        ),
    ));
    cases.push((
        "trigger_shape_tick",
        bench.scrub_tick(ValueRef::AudioTriggerShape(glow.clone(), 0, AudioShapeParam::Attack), ScrubValue::Scalar(20.0)),
    ));
    cases.push(("layer_gain_tick", bench.scrub_tick(ValueRef::LayerAudioGain(glow.clone()), ScrubValue::Scalar(-6.0))));
    cases.push(("send_gain_tick", bench.scrub_tick(ValueRef::AudioSendGain(send_id), ScrubValue::Scalar(-3.0))));
    cases.push(("band_split_tick", bench.scrub_tick(ValueRef::AudioCrossover(BandDivider::Low), ScrubValue::Scalar(300.0))));
    cases.push(("relight_tick", bench.scrub_tick(ValueRef::RelightParam(fx(0), UiRelightField::Gain), ScrubValue::Scalar(0.8))));
    {
        // The editor canvas's unbound node-param scrub (`app_render.rs`
        // `GraphEditCommand` arm): one `MutateProject` per move, first move `Overrides | Shapes`. The graph
        // editor window has no headless driver, so the command is built as
        // that arm builds it.
        let target = manifold_core::GraphTarget::Effect(bench.ui.project.timeline.layers[0].effects.as_ref().unwrap()[0].id.clone());
        let default = crate::graph_target::owner_default(&bench.ui.project, &target).expect("Mirror has a catalog graph");
        let (node, name) = default
            .nodes
            .iter()
            .find_map(|n| {
                n.params
                    .iter()
                    .find(|(_, v)| matches!(v, manifold_core::effect_graph_def::SerializedParamValue::Float { .. }))
                    .map(|(k, _)| (n.id, k.clone()))
            })
            .expect("Mirror's graph has a float node param");
        let mut cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
            target,
            node,
            name,
            manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.42 },
            default,
        );
        cases.push(("node_param_tick", bench.measure(|b| {
            b.send(vec![ContentCommand::mutate(crate::content_command::touch::OVERRIDES_SHAPES, Box::new(move |p| {
                use manifold_editing::command::Command;
                cmd.execute(p);
            }))]);
            b.ticks(1);
        })));
    }
    {
        // The editor mapping drawer's reshape preview (`editor_bridge.rs`
        // `preview_mapping`): one `MutateProject` (Shapes) per move.
        let target = manifold_core::GraphTarget::Effect(bench.ui.project.timeline.layers[0].effects.as_ref().unwrap()[1].id.clone());
        let seed = crate::editor_bridge::seed_def_for_project(&bench.ui.project, &target);
        let pid = mod_params[0].clone();
        let edit = manifold_editing::commands::effects::BindingMappingEdit { min: Some(0.2), ..Default::default() };
        cases.push(("mapping_drawer_tick", bench.measure(|b| {
            b.send(vec![ContentCommand::mutate(crate::content_command::touch::OVERRIDES_SHAPES, Box::new(move |p| {
                crate::editor_bridge::build_mapping_command(&target, &pid, edit, seed).execute(p);
            }))]);
            b.ticks(1);
        })));
    }
    // Inbound OSC and Ableton values, queued where their receiver threads
    // queue them, then one real tick. Each case proves its value landed.
    let glow_opacity = |b: &Bench| {
        b.content.engine.project().and_then(|p| p.timeline.find_layer_by_id("glow").map(|(_, l)| l.opacity))
    };
    let before = glow_opacity(&bench);
    cases.push(("osc_router_tick", bench.measure(|b| {
        b.content.osc_receiver.inject("/layer/glow/opacity", &[0.25]);
        b.ticks(1);
    })));
    let after = glow_opacity(&bench);
    assert!(after == Some(0.25) && before != after, "OSC value did not land: {before:?} -> {after:?}");

    let abl_param = lfo_params[1].clone();
    let mirror_base = |b: &Bench| {
        b.content.engine.project().and_then(|p| {
            p.timeline.find_layer_by_id("glow").map(|(_, l)| l.effects.as_ref().unwrap()[0].get_base_param(&abl_param))
        })
    };
    let before = mirror_base(&bench);
    cases.push(("ableton_bridge_tick", bench.measure(|b| {
        b.content.ableton_bridge.inject_param_value((0, 0, 1), 0.9);
        b.ticks(1);
    })));
    let after = mirror_base(&bench);
    assert!(before != after, "Ableton value did not land: {before:?} -> {after:?}");

    check_or_record(&cases);
}

/// INV-17: every `data_version` move and every content-originated change
/// outside the live scalars publishes a project snapshot.
#[test]
fn content_mutate_publishes() {
    let mut bench = Bench::new();
    let published = |b: &mut Bench| -> Option<std::sync::Arc<Project>> {
        b.content.tick_frame(&b.state_tx);
        b.state_rx.try_iter().filter_map(|s| s.project_snapshot).last()
    };
    let mapping_status = |p: &Project| {
        p.timeline.layers[0].effects.as_ref().unwrap()[0].ableton_mappings.as_ref().unwrap()[0].status
    };

    // A content-side Mappings write: discovery validation with no session
    // marks the mapping Dormant, and the UI receives the new status.
    bench.content.ableton_bridge.request_validation();
    let snap = published(&mut bench).expect("a content-side Mappings write publishes");
    assert_eq!(mapping_status(&snap), manifold_core::ableton_mapping::AbletonMappingStatus::Dormant);

    // Undo of a Values edit (slider) and of a Shapes edit (driver trim).
    let fx = |i| GraphParamTarget::Effect(i);
    let p = |s: &str| std::borrow::Cow::Owned(s.to_string());
    let lfo = param_ids(&bench.ui.project.timeline.layers[0].effects.as_ref().unwrap()[0]);
    let modp = param_ids(&bench.ui.project.timeline.layers[0].effects.as_ref().unwrap()[1]);
    for (address, value) in [
        (ValueRef::Param(fx(1), p(&modp[0])), ScrubValue::Scalar(0.6)),
        (ValueRef::Trim(TrimKind::Driver, fx(0), p(&lfo[0])), ScrubValue::Range(0.2, 0.7)),
    ] {
        for phase in [ScrubPhase::Begin, ScrubPhase::Move(value), ScrubPhase::Commit] {
            let sent = bench.ui.dispatch(PanelAction::Scrub(address.clone(), phase));
            bench.send(sent);
        }
        let _ = published(&mut bench);
        bench.send(vec![ContentCommand::Undo]);
        assert!(published(&mut bench).is_some(), "undo of {address:?} publishes");
    }
}

/// `FrameAlloc` is left out: it counts only in a `work-trace` build, and this
/// test must read the same with and without that feature.
fn check_or_record(cases: &[(&str, WorkSnapshot)]) {
    let text = std::fs::read_to_string(CEILINGS).unwrap_or_else(|_| "{}".into());
    let mut ceilings: serde_json::Value = serde_json::from_str(&text).expect("work_ceilings.json parses");
    if recording() {
        let mut section = serde_json::Map::new();
        for (name, counts) in cases {
            let mut kinds = serde_json::Map::new();
            for (kind, n) in counts.nonzero().filter(|(k, _)| *k != WorkKind::FrameAlloc) {
                kinds.insert(kind.name().to_string(), serde_json::json!(n));
            }
            section.insert((*name).to_string(), serde_json::Value::Object(kinds));
        }
        ceilings
            .as_object_mut()
            .expect("ceilings file is an object")
            .insert("content_reaction_budget".into(), serde_json::Value::Object(section));
        std::fs::write(CEILINGS, serde_json::to_string_pretty(&ceilings).unwrap() + "\n").expect("write ceilings");
        return;
    }
    let mut over = Vec::new();
    for (name, counts) in cases {
        for (kind, n) in counts.nonzero().filter(|(k, _)| *k != WorkKind::FrameAlloc) {
            let ceiling = ceilings
                .pointer(&format!("/content_reaction_budget/{name}/{}", kind.name()))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if n > ceiling {
                over.push(format!("{name}: {}={n} > {ceiling}", kind.name()));
            }
        }
    }
    assert!(over.is_empty(), "content work over ceiling (re-record only with {RECORD_ENV}=1):\n{}", over.join("\n"));
}

/// D24's oracle: a tempo ramp through a recording session, beat and time per
/// tick, against values recorded from today's code. The external tempo is fed
/// the way a Link peer feeds it (`set_live_external_tempo`).
#[test]
fn tempo_ramp_matches_today() {
    let mut project = budget_project();
    project.settings.bpm = Bpm(120.0);
    let mut content = crate::headless_harness::headless_content_thread(project, 64, 64);
    content.timer.set_frame_clocked(true);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    content.handle_command(ContentCommand::SetRecording(true));
    content.handle_command(ContentCommand::Play);
    const TICKS: usize = 120;
    let start = content.editing_service.stamps();
    let mut rows = Vec::with_capacity(TICKS);
    for i in 0..TICKS {
        let bpm = 120.0 + 20.0 * i as f32 / (TICKS - 1) as f32;
        content.engine.set_live_external_tempo(true, Bpm(bpm), TempoPointSource::Link);
        content.tick_frame(&state_tx);
        rows.push([content.engine.current_beat().0, content.engine.current_time().0]);
    }
    // The session's in-tick writes moved no stamp; its end is one Settings stamp.
    let session = content.editing_service.stamps();
    assert_eq!(session, start, "in-session tempo points and BPM are audited tick writes");
    content.handle_command(ContentCommand::Pause);
    let ended = content.editing_service.stamps();
    assert_eq!(ended.moved_since(&session), ChangeSet::of(&[Part::Settings]), "session end stamps Settings only");
    assert_eq!(ended.get(Part::Settings), session.get(Part::Settings) + 1, "session end stamps Settings once");
    let points = content.engine.project().map_or(0, |p| p.tempo_map.point_count());
    let observed = serde_json::json!({ "rows": rows, "tempo_points_after_session": points });
    if recording() {
        std::fs::write(TEMPO_ORACLE, serde_json::to_string_pretty(&observed).unwrap() + "\n").expect("write tempo oracle");
        return;
    }
    let text = std::fs::read_to_string(TEMPO_ORACLE).expect("tempo oracle recorded (run once with MANIFOLD_RECORD_WORK=1)");
    let today: serde_json::Value = serde_json::from_str(&text).expect("tempo oracle parses");
    assert_eq!(today["tempo_points_after_session"], observed["tempo_points_after_session"]);
    let today_rows = today["rows"].as_array().expect("rows");
    assert_eq!(today_rows.len(), rows.len());
    for (i, (want, got)) in today_rows.iter().zip(&rows).enumerate() {
        for (j, label) in ["beat", "time"].iter().enumerate() {
            let w = want[j].as_f64().expect("number");
            assert!((w - got[j]).abs() <= 1e-9, "tick {i}: {label} {} != today's {w}", got[j]);
        }
    }
}

/// INV-22: a clock wobble is not an edit. A followed tempo that wobbles by
/// ±0.01 BPM, outside a recording session, moves no stamp, publishes no
/// snapshot and clones no project.
#[test]
fn clock_wobble_is_not_an_edit() {
    let mut project = budget_project();
    project.settings.bpm = Bpm(120.0);
    let mut content = crate::headless_harness::headless_content_thread(project, 64, 64);
    content.timer.set_frame_clocked(true);
    let (state_tx, state_rx) = crossbeam_channel::unbounded::<ContentState>();
    content.handle_command(ContentCommand::Play);
    content.tick_frame(&state_tx);
    let _ = state_rx.try_iter().count();
    let before = content.editing_service.stamps();
    let work = thread_work_snapshot();
    for i in 0..120 {
        let wobble = if i % 2 == 0 { 0.01 } else { -0.01 };
        content.engine.set_live_external_tempo(true, Bpm(120.0 + wobble), TempoPointSource::Link);
        content.tick_frame(&state_tx);
    }
    let work = thread_work_snapshot().since(&work);
    assert_eq!(content.editing_service.stamps(), before, "a wobble moved a stamp");
    assert_eq!(state_rx.try_iter().filter(|s| s.project_snapshot.is_some()).count(), 0, "a wobble published a snapshot");
    assert_eq!(work.get(WorkKind::ProjectClone), 0, "a wobble cloned the project");
}

/// INV-20: perturb every Values and Shapes field `LIVE_SCALAR_CONSUMERS` lists,
/// through the same `MutateProject` a live drag sends, and prove its consumer
/// sees the new value on the next tick: read fresh from the project the tick
/// renders, captured into the published modulation snapshot, or reached
/// through the listed reaction row (send gain -> `sync_send_gains`).
#[test]
fn shapes_fields_reach_consumers() {
    use crate::content_command::touch;
    use manifold_core::change::LIVE_SCALAR_CONSUMERS;
    let mut bench = Bench::new();
    let fx_id = |b: &Bench, i: usize| b.content.engine.project().unwrap().timeline.layers[0].effects.as_ref().unwrap()[i].id.clone();
    fn glow(p: &mut Project) -> &mut manifold_core::layer::Layer {
        &mut p.timeline.layers[0]
    }
    let mirror_pid = param_ids(&bench.content.engine.project().unwrap().timeline.layers[0].effects.as_ref().unwrap()[2])[1].clone();
    let lfo_pid = param_ids(&bench.content.engine.project().unwrap().timeline.layers[0].effects.as_ref().unwrap()[0])[0].clone();
    let mut covered: Vec<&str> = Vec::new();

    // One tick after the write; returns the published modulation snapshot
    // applied onto a copy of the project the tick rendered.
    let tick_and_capture = |b: &mut Bench, touches, f: Box<dyn FnOnce(&mut Project) + Send>| -> Project {
        b.send(vec![ContentCommand::mutate(touches, f)]);
        b.content.tick_frame(&b.state_tx);
        let snap = b.state_rx.try_iter().filter_map(|s| s.modulation_snapshot).last();
        let mut seen = b.content.engine.project().unwrap().clone();
        if let Some(s) = snap {
            s.apply(&mut seen);
        }
        seen
    };

    // Values, captured for the UI and rendered fresh.
    let pid = mirror_pid.clone();
    let seen = tick_and_capture(&mut bench, touch::VALUES, Box::new(move |p| {
        glow(p).effects.as_mut().unwrap()[2].set_base_param_by_id(&pid, 0.37);
    }));
    assert_eq!(seen.timeline.layers[0].effects.as_ref().unwrap()[2].get_param(&mirror_pid), 0.37);
    covered.push("param base and value");

    let seen = tick_and_capture(&mut bench, touch::VALUES, Box::new(|p| {
        glow(p).opacity = 0.3;
        p.settings.master_opacity = 0.6;
    }));
    assert_eq!((seen.timeline.layers[0].opacity, seen.settings.master_opacity), (0.3, 0.6));
    covered.push("layer and master opacity");

    let seen = tick_and_capture(&mut bench, touch::VALUES, Box::new(|p| p.settings.macro_bank.slots[0].value = 0.61));
    assert_eq!(seen.settings.macro_bank.slots[0].value, 0.61);
    covered.push("macro values");

    let seen = tick_and_capture(&mut bench, touch::VALUES, Box::new(|p| {
        p.settings.led_brightness = 0.4;
        p.settings.led_gain = 0.8;
    }));
    assert_eq!((seen.settings.led_brightness, seen.settings.led_gain), (0.4, 0.8));
    covered.push("led_brightness, led_gain");

    // Shapes. A driver trimmed to a point holds the modulated value still:
    // the modulation pass read the new shape on the very next tick.
    let pid = lfo_pid.clone();
    let first = tick_and_capture(&mut bench, touch::SHAPES, Box::new(move |p| {
        let d = &mut glow(p).effects.as_mut().unwrap()[0].drivers.as_mut().unwrap()[0];
        let _ = &pid;
        d.trim_min = 0.8;
        d.trim_max = 0.8;
    }));
    let second = tick_and_capture(&mut bench, touch::VALUES, Box::new(|_| {}));
    let v = |p: &Project| p.timeline.layers[0].effects.as_ref().unwrap()[0].get_param(&lfo_pid);
    assert_eq!(v(&first), v(&second), "a point-trimmed LFO must hold still from the first tick");
    covered.push("driver, envelope and audio-mod shapes");

    let seen = tick_and_capture(&mut bench, touch::SHAPES, Box::new(|p| glow(p).clip_triggers[0].shape.sensitivity = 0.42));
    assert_eq!(seen.timeline.layers[0].clip_triggers[0].shape.sensitivity, 0.42);
    covered.push("clip-trigger shape");

    let pid = lfo_pid.clone();
    let seen = tick_and_capture(&mut bench, touch::SHAPES, Box::new(move |p| {
        let spec = &mut glow(p).effects.as_mut().unwrap()[0].params.get_mut(&pid).unwrap().spec;
        spec.min = 0.5;
        spec.max = 0.5;
    }));
    assert_eq!(v(&seen), 0.5, "the modulated value must land inside the reshaped range on the next tick");
    covered.push("param-mapping reshape");

    let seen = tick_and_capture(&mut bench, touch::SHAPES, Box::new(|p| glow(p).effects.as_mut().unwrap()[1].relight_params.light_x = 0.25));
    assert_eq!(seen.timeline.layers[0].effects.as_ref().unwrap()[1].relight_params.light_x, 0.25);
    covered.push("relight_params");

    let target = manifold_core::GraphTarget::Effect(fx_id(&bench, 1));
    let seed = crate::editor_bridge::seed_def_for_project(bench.content.engine.project().unwrap(), &target).expect("Bloom has a catalog graph");
    let (node, name) = seed
        .nodes
        .iter()
        .find_map(|n| n.params.iter().find(|(_, v)| matches!(v, manifold_core::effect_graph_def::SerializedParamValue::Float { .. })).map(|(k, _)| (n.id, k.clone())))
        .expect("Bloom's graph has a float node param");
    let written = manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.123 };
    let (w, nm) = (written.clone(), name.clone());
    let seen = tick_and_capture(&mut bench, touch::OVERRIDES_SHAPES, Box::new(move |p| {
        let fx = &mut glow(p).effects.as_mut().unwrap()[1];
        let mut graph = seed;
        graph.nodes.iter_mut().find(|n| n.id == node).unwrap().params.insert(nm, w);
        fx.graph = Some(graph);
    }));
    let graph = seen.timeline.layers[0].effects.as_ref().unwrap()[1].graph.as_ref().expect("override landed");
    assert_eq!(graph.nodes.iter().find(|n| n.id == node).unwrap().params.get(&name), Some(&written));
    covered.push("unbound graph node param values");

    let seen = tick_and_capture(&mut bench, touch::SHAPES, Box::new(|p| glow(p).audio_gain_db = -9.0));
    assert_eq!(seen.timeline.layers[0].audio_gain_db, -9.0);
    covered.push("layer.audio_gain_db");

    let before = bench.content.audio_mod_runtime.send_gain_syncs.get();
    let seen = tick_and_capture(&mut bench, touch::SHAPES, Box::new(|p| p.audio_setup.sends[0].gain_db = -4.0));
    assert_eq!(seen.audio_setup.sends[0].gain_db, -4.0);
    assert!(bench.content.audio_mod_runtime.send_gain_syncs.get() > before, "the cached GainBank must be refreshed by its reaction row");
    covered.push("audio send gain_db");

    let seen = tick_and_capture(&mut bench, touch::SHAPES, Box::new(|p| {
        p.audio_setup.low_hz = 180.0;
        p.audio_setup.mid_hz = 2400.0;
    }));
    assert_eq!((seen.audio_setup.low_hz, seen.audio_setup.mid_hz), (180.0, 2400.0));
    covered.push("band split low_hz, mid_hz");

    let listed: Vec<&str> = LIVE_SCALAR_CONSUMERS.iter().map(|(f, part, _)| {
        assert!(matches!(part, Part::Values | Part::Shapes));
        *f
    }).collect();
    assert_eq!(covered, listed, "every live scalar row is perturbed, in table order");
}
