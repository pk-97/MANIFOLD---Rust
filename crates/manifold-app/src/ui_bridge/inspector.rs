//! Inspector-related dispatch: effect params, drivers, envelopes, generator params,
//! master/layer/clip chrome, slider interactions.

// `dispatch_inspector` (the first-non-unhandled chain over the six
// `dispatch/` handler modules) was RETIRED in P-D / D-D1: with `PanelAction`
// now a flat sum, the top-level `ui_bridge::dispatch` routes each domain arm
// directly to its handler, and the compiler proves routing totality. Keeping
// the chain — or its `dispatch_chain_completeness` invariant — beside the
// exhaustive sum match would be a second copy of the routing, exactly the
// parallel-old-path the design forbids. The tests below now drive dispatch
// through the real `ui_bridge::dispatch` entry point (see `Harness::dispatch`).

#[cfg(test)]
mod scene_card_convergence_tests {
    //! SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a gates: a fog-density
    //! drag session dispatched through the REAL `dispatch_inspector` entry
    //! point (the same one `ui_bridge::dispatch` routes `PanelAction::
    //! ParamSnapshot`/`ParamChanged`/`ParamCommit` to) yields exactly ONE
    //! undo unit whose undo restores the pre-drag value, and the write
    //! lands in the layer's own instance def (mirrors project.rs's
    //! `scene_layer_project` SceneStarter fixture — C7's precedent for
    //! testing a scene write against the layer's REAL def, not a bare
    //! `EffectGraphDef` literal).
    // Test-only imports, relocated from file scope at the P-D landing: this
    // file's only production content was `dispatch_inspector`, retired in D-D1,
    // so these were unused in a non-test build. (The former `use super::*` here
    // is dropped: the inspector module now has no non-test items to re-export.)
    use manifold_ui::{AudioSetupAction, EditingAction, MappingAction, ModulationAction, ParamsAction};
    use manifold_core::effects::ParameterDriver;
    use manifold_core::types::{BeatDivision, DriverWaveform};
    use manifold_core::LayerId;
    use manifold_ui::{DriverConfigAction, PanelAction, ScrubPhase, ScrubValue, ValueRef};
    use crate::ui_bridge::DispatchResult;
    use crate::app::SelectionState;
    use crate::content_command::ContentCommand;
    use crate::ui_root::UIRoot;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    use manifold_core::project::Project;
    use manifold_core::types::LayerType;
    use manifold_renderer::node_graph::scene_vm::{AtmosphereVm, SceneVm};

    /// A fresh SceneStarter generator layer + its `render_scene` node id —
    /// same fixture `project.rs`'s `scene_layer_project` uses. SceneStarter
    /// ships a wired `node.atmosphere` (fog_density 0.04, height_falloff
    /// 0.3), so `AtmosphereVm::from_def` resolves `Wired` without any
    /// synthetic graph surgery.
    fn scene_layer_project() -> (Project, LayerId) {
        let mut project = Project::default();
        let idx = project.timeline.add_layer(
            "Scene",
            LayerType::Generator,
            PresetTypeId::from_string("SceneStarter".to_string()),
        );
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        (project, layer_id)
    }

    /// The layer's fog-density write address, read straight off the SAME
    /// `SceneVm::from_def` production code walks (`state_sync.rs`'s VM
    /// builder) — never hand-picked.
    fn fog_density_addr(project: &Project, layer_id: &LayerId) -> manifold_core::effect_graph_def::EffectGraphDef {
        let (_, layer) = project.timeline.find_layer_by_id(layer_id).unwrap();
        layer.generator_graph().cloned().unwrap_or_else(|| {
            manifold_renderer::node_graph::bundled_preset_def(&layer.generator_type().clone())
                .cloned()
                .expect("SceneStarter is a bundled preset")
        })
    }

    fn density_node_id(def: &manifold_core::effect_graph_def::EffectGraphDef) -> u32 {
        let vm = SceneVm::from_def(def).expect("SceneStarter resolves as a scene");
        let AtmosphereVm::Wired(a) = vm.atmosphere else {
            panic!("SceneStarter's atmosphere must be Wired");
        };
        a.node_doc_id
    }

    #[allow(clippy::type_complexity)]
    struct Harness {
        content_tx: crossbeam_channel::Sender<ContentCommand>,
        content_rx: crossbeam_channel::Receiver<ContentCommand>,
        content_state: crate::content_state::ContentState,
        ui: UIRoot,
        selection: SelectionState,
        active_layer: Option<LayerId>,
        // `dispatch_inspector` ignores `user_prefs`, but `DispatchCtx` requires
        // the field — supply an in-memory instance (wiring, not behavior).
        user_prefs: crate::user_prefs::UserPrefs,
        scrub: crate::ui_bridge::ScrubState,
    }

    impl Harness {
        fn new(active_layer: Option<LayerId>) -> Self {
            let (content_tx, content_rx) = crossbeam_channel::unbounded();
            Self {
                content_tx,
                content_rx,
                content_state: crate::content_state::ContentState::default(),
                ui: UIRoot::new(),
                selection: manifold_ui::UIState::new(),
                active_layer,
                user_prefs: crate::user_prefs::UserPrefs::in_memory(),
                scrub: crate::ui_bridge::ScrubState::default(),
            }
        }

        fn dispatch(&mut self, action: &PanelAction, project: &mut Project) -> DispatchResult {
            let mut ctx = crate::ui_bridge::DispatchCtx {
                project,
                content_tx: &self.content_tx,
                content_state: &self.content_state,
                ui: &mut self.ui,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer,
                user_prefs: &mut self.user_prefs,
                editor_target: None,
                scrub: &mut self.scrub,
            };
            crate::ui_bridge::dispatch(action, &mut ctx)
        }

        fn drain(&self) -> Vec<ContentCommand> {
            self.content_rx.try_iter().collect()
        }

        /// `dispatch`'s twin with an explicit `editor_target` — the graph
        /// editor's own identity-addressed entry point
        /// (`resolve_effect_id`/`editor_dispatch_context`, `ui_bridge/mod.rs`):
        /// a `Some(GraphTarget::Effect(id))` resolves that exact instance
        /// (master, layer, or clip) regardless of `GraphParamTarget`'s
        /// positional index or the ambient `last_effect_tab`. `row_dispatch`
        /// uses this to reach a master effect and a layer effect through the
        /// identical dispatch call — the only production path that can
        /// address a specific scope without driving a real pointer-down
        /// through `InspectorPanel::handle_click` (private to manifold-ui,
        /// P2's own test surface).
        fn dispatch_with_editor(
            &mut self,
            action: &PanelAction,
            project: &mut Project,
            editor_target: Option<&manifold_core::GraphTarget>,
        ) -> DispatchResult {
            let mut ctx = crate::ui_bridge::DispatchCtx {
                project,
                content_tx: &self.content_tx,
                content_state: &self.content_state,
                ui: &mut self.ui,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer,
                user_prefs: &mut self.user_prefs,
                editor_target,
                scrub: &mut self.scrub,
            };
            crate::ui_bridge::dispatch(action, &mut ctx)
        }
    }

    /// Undo-race repro (param-feed regression, 2026-07-18): since
    /// `ac96c65c` the content thread ships a `ModulationSnapshot` EVERY
    /// tick and `app_render.rs` applies it to `local_project`
    /// unconditionally (only overlay drags gate it). The restore guard
    /// (`ActiveInspectorDrag`) has no Macro variant, so a stale snapshot
    /// landing mid-drag stomps the in-flight value back to pre-drag; the
    /// commit handler then sees old == new and emits NO undo command.
    #[test]
    fn macro_drag_survives_a_mid_gesture_modulation_snapshot() {
        let mut project = Project::default();
        project.settings.macro_bank.slots[0].value = 0.2;

        // The content thread's view is still pre-drag when it captures.
        let mut stale = crate::content_state::ModulationSnapshot::empty();
        stale.capture_into(&project);

        let mut h = Harness::new(None);
        h.dispatch(&PanelAction::Scrub(ValueRef::Macro(0), ScrubPhase::Begin), &mut project);
        h.dispatch(&PanelAction::Scrub(ValueRef::Macro(0), ScrubPhase::Move(ScrubValue::Scalar(0.8))), &mut project);
        h.drain();

        // What the UI frame drain now does every tick (app_render.rs ~line
        // 868): apply the snapshot, then restore the guarded gesture (the macro
        // rides the P-I `active` gesture now, restored via `restore_dragged`).
        stale.apply(&mut project);
        h.scrub.restore_dragged(&mut project);

        h.dispatch(&PanelAction::Scrub(ValueRef::Macro(0), ScrubPhase::Commit), &mut project);
        let cmds = h.drain();
        assert!(
            cmds.iter().any(|c| matches!(c, ContentCommand::Execute(_))),
            "a completed macro drag must produce an undo-tracked command; got {} commands",
            cmds.len()
        );
    }

    /// BUG-246 (trim family): while a modulation trim handle is dragged with
    /// playback running, a full snapshot is accepted every frame
    /// (`app_render.rs` ~808 replaces `local_project`, then the unguarded
    /// per-frame `sync_inspector_data` at ~3373 reconfigures cards from it).
    /// Before the `Trim` variant, `drag.apply` had no arm for trim, so the
    /// in-flight `[min,max]` reverted to the snapshot's stale range every
    /// frame — the handle jumped/vanished mid-gesture. The restore must write
    /// the dragged range back through the driver's `trim_min/trim_max`, the
    /// same store `TrimChanged`'s driver dual-edit uses.
    #[test]
    fn driver_trim_range_survives_a_mid_gesture_snapshot() {
        use crate::ui_bridge::scrub::{ResolvedScrub, ScrubState};
        let (mut project, layer_id) = scene_layer_project();
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let pid: manifold_core::effects::ParamId = std::borrow::Cow::Owned("density".to_string());

        // Arm a driver carrying the user's in-flight trim range (0.3..0.9).
        project.with_preset_graph_mut(&target, |inst| {
            inst.drivers = Some(vec![ParameterDriver {
                param_id: pid.clone(),
                beat_division: manifold_core::types::BeatDivision::Quarter,
                waveform: manifold_core::types::DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.0,
                trim_min: 0.3,
                trim_max: 0.9,
                reversed: false,
                free_period_beats: None,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
        });

        // The content thread's stale snapshot: same driver, DEFAULT trim.
        let mut stale = project.clone();
        stale.with_preset_graph_mut(&target, |inst| {
            if let Some(ds) = inst.drivers.as_mut() {
                ds[0].trim_min = 0.0;
                ds[0].trim_max = 1.0;
            }
        });

        // app_render mid-drag: local_project := stale, then restore the gesture
        // through the P-I `active` scrub state (the production restore path).
        let scrub = ScrubState {
            active: Some(ResolvedScrub::Trim {
                kind: manifold_ui::panels::TrimKind::Driver,
                target: target.clone(),
                ableton_target: None,
                param_id: pid.clone(),
                baseline: (0.3, 0.9),
                live: (0.3, 0.9),
            }),
            ..Default::default()
        };
        let mut local = stale;
        scrub.restore_dragged(&mut local);

        let (mn, mx) = local
            .with_preset_graph_mut(&target, |inst| {
                let d = &inst.drivers.as_ref().unwrap()[0];
                (d.trim_min, d.trim_max)
            })
            .expect("generator instance resolves");
        assert!(
            (mn - 0.3).abs() < 1e-6 && (mx - 0.9).abs() < 1e-6,
            "trim range must survive the snapshot stomp; got ({mn}, {mx}) instead of (0.3, 0.9)"
        );
    }

    /// Phase-1 baseline for the undo/redo audit (Peter 2026-07-19: undo/redo
    /// "broken, out of order, or just don't respond" across sliders, buttons,
    /// toggles, clips, trims). Every undoable gesture family gets two probes:
    ///
    /// - CLEAN: gesture → exactly ONE undo-tracked `Execute` → execute/undo/
    ///   redo round-trips the probed value through a REAL `EditingService`
    ///   (the content thread's own gateway), and the undo stack grows by
    ///   exactly one per gesture.
    /// - STOMP (drag trios only): a full project snapshot lands mid-gesture
    ///   (data_version bump from any concurrent command — playback, MIDI
    ///   phantom commit, another gesture), simulated exactly the way
    ///   app_render.rs ~808-817 applies it: replace the local project, then
    ///   restore the guarded drag. Families without an `ActiveInspectorDrag`
    ///   variant lose the in-flight value here — the commit then sees
    ///   old == new and emits NO undo entry ("doesn't respond").
    mod undo_baseline {
        use super::*;
        use manifold_editing::service::EditingService;

        /// The content-thread side of the loop: a real `EditingService` over
        /// its own project, driven exactly the way content_commands.rs drives
        /// it (`Execute` → `service.execute`, `ExecuteBatch` → `execute_batch`,
        /// `MutateProject(Live)` → plain closure application, no undo entry).
        struct ContentSide {
            project: Project,
            service: EditingService,
            undo_depth: usize,
        }

        impl ContentSide {
            fn new(project: &Project) -> Self {
                Self {
                    project: project.clone(),
                    service: EditingService::new(),
                    undo_depth: 0,
                }
            }

            /// Apply every drained command the way the content thread would.
            /// Returns how many undo-tracked commands landed.
            fn apply(&mut self, cmds: Vec<ContentCommand>) -> usize {
                let mut n = 0;
                for c in cmds {
                    match c {
                        ContentCommand::Execute(cmd) => {
                            self.service.execute(cmd, &mut self.project);
                            self.undo_depth += 1;
                            n += 1;
                        }
                        ContentCommand::ExecuteBatch(cmds, desc) => {
                            let k = cmds.len();
                            self.service.execute_batch(cmds, desc, &mut self.project);
                            self.undo_depth += k.max(1);
                            n += 1;
                        }
                        ContentCommand::MutateProject(f) | ContentCommand::MutateProjectLive(f) => {
                            f(&mut self.project);
                        }
                        _ => {}
                    }
                }
                n
            }
        }

        /// Full gesture → undo → redo cycle assertion: the gesture must emit
        /// exactly one undo-tracked command; executing it lands `after`;
        /// undo restores `before`; redo reapplies `after`; stack grows by 1.
        fn assert_undo_cycle<P>(
            side: &mut ContentSide,
            cmds: Vec<ContentCommand>,
            probe: impl Fn(&Project) -> P,
            before: P,
            after: P,
            label: &str,
        ) where
            P: PartialEq + std::fmt::Debug,
        {
            let depth0 = side.undo_depth;
            let landed = side.apply(cmds);
            assert_eq!(
                landed, 1,
                "{label}: gesture must emit exactly ONE undo-tracked Execute; got {landed}"
            );
            assert_eq!(
                side.undo_depth,
                depth0 + 1,
                "{label}: undo stack must grow by exactly one per gesture"
            );
            assert_eq!(probe(&side.project), after, "{label}: execute must land the new value");
            assert!(side.service.undo(&mut side.project), "{label}: undo must be available");
            assert_eq!(
                probe(&side.project),
                before,
                "{label}: undo must restore the pre-gesture value"
            );
            assert!(side.service.redo(&mut side.project), "{label}: redo must be available");
            assert_eq!(probe(&side.project), after, "{label}: redo must reapply the value");
        }

        /// Mirror app_render's mid-gesture full-snapshot acceptance: replace
        /// the local project with the stale pre-gesture one, then restore the
        /// guarded drag (app_render.rs ~808-817).
        fn snapshot_stomp(h: &Harness, stale: &Project) -> Project {
            let mut p = stale.clone();
            // Restore whichever gesture is live — the interim
            // `active_inspector_drag` families or the P-I `active` gesture.
            h.scrub.restore_dragged(&mut p);
            p
        }

        /// Drive a drag trio and assert the undo cycle, clean or stomped.
        /// `gesture` runs Snapshot + Changed ticks (NOT the commit); `commit`
        /// dispatches the commit action and returns the drained commands.
        fn trio_cycle<P>(
            label: &str,
            mut project: Project,
            h: &mut Harness,
            gesture: impl Fn(&mut Harness, &mut Project),
            commit: impl Fn(&mut Harness, &mut Project) -> DispatchResult,
            probe: impl Fn(&Project) -> P,
            before: P,
            after: P,
            stomp: bool,
        ) where
            P: PartialEq + std::fmt::Debug,
        {
            let stale = project.clone();
            let mut side = ContentSide::new(&project);
            gesture(h, &mut project);
            // Live ticks reach the content thread as non-undoable writes.
            side.apply(h.drain());
            if stomp {
                project = snapshot_stomp(h, &stale);
            }
            commit(h, &mut project);
            let label = if stomp { format!("{label} [stomp]") } else { label.to_string() };
            assert_undo_cycle(&mut side, h.drain(), probe, before, after, &label);
        }

        // ── Fixtures ─────────────────────────────────────────────

        fn gpt() -> manifold_ui::GraphParamTarget {
            manifold_ui::GraphParamTarget::Generator
        }

        /// Resolve a REAL exposed param id for the scene layer's fog_density
        /// node. P2 slice 2a (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md):
        /// fog_density is a P1-stamped exposed card param from creation —
        /// `migrate_scene_exposures` runs on every bundled generator preset
        /// at load (`bundled_generator_presets.rs`'s own comment), so
        /// SceneStarter's atmosphere node is ALREADY exposed, no
        /// expose-then-arm dance through the scene panel's (now-dead
        /// this slice, per BUG_BACKLOG.md) synthesized-id path needed —
        /// this is byte-for-byte the same "real exposed param" every other
        /// `undo_baseline` fixture below already exercises for effects/
        /// generators, just using the scene layer's own atmosphere param as
        /// the specimen. `h` is unused now that no `PanelAction` dispatch is
        /// needed to arm anything; kept in the signature so every call site
        /// below reads uniformly.
        fn materialized_param(
            _h: &mut Harness,
            project: &mut Project,
            layer_id: &LayerId,
        ) -> manifold_core::effects::ParamId {
            let catalog_default = fog_density_addr(project, layer_id);
            let node_doc_id = density_node_id(&catalog_default);
            let (_, layer) = project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .expect("layer resolves");
            let inst = layer.gen_params_or_init();
            // A freshly-init'd instance still TRACKS its catalog preset
            // (`graph: None`) — `binding_id_for_node_param` only resolves
            // against an instance's OWN graph override, so the effective-def
            // fallback (the same one BUG-260's `display_value` uses) is
            // mandatory here, not optional.
            let real = inst
                .binding_id_for_node_param(node_doc_id, "fog_density")
                .or_else(|| {
                    manifold_core::effects::binding_id_for_node_param_in(
                        &catalog_default,
                        node_doc_id,
                        "fog_density",
                    )
                })
                .expect("SceneStarter's fog_density must already be exposed by P1 stamping");
            // The OLD synth-id `DriverToggle` dispatch this fixture used to
            // run did double duty: it exposed AND armed an enabled driver in
            // one shot (BUG-249's "expose-then-arm"). `driver_toggle_atomic`/
            // `driver_trim_clean`/`driver_trim_stomp` below assume that
            // pre-armed driver as their "before" state — reconstruct it
            // directly (no dispatch needed now that exposure is a given).
            if inst.drivers.as_ref().is_none_or(|ds| ds.is_empty()) {
                inst.drivers = Some(vec![manifold_core::effects::ParameterDriver {
                    param_id: std::borrow::Cow::Owned(real.clone()),
                    beat_division: manifold_core::types::BeatDivision::Quarter,
                    waveform: manifold_core::types::DriverWaveform::Sine,
                    enabled: true,
                    phase: 0.0,
                    base_value: 0.0,
                    trim_min: 0.0,
                    trim_max: 1.0,
                    reversed: false,
                    free_period_beats: None,
                    legacy_param_index: None,
                    is_paused_by_user: false,
                }]);
            }
            std::borrow::Cow::Owned(real)
        }

        /// Immutable read of a layer's generator instance — the probe-side
        /// counterpart of `with_preset_graph_mut` (no immutable variant
        /// exists, and probes only get `&Project`).
        fn gen_inst<'p>(
            p: &'p Project,
            layer_id: &LayerId,
        ) -> &'p manifold_core::effects::PresetInstance {
            let (_, layer) = p.timeline.find_layer_by_id(layer_id).expect("layer resolves");
            layer.gen_params().expect("generator instance materialized")
        }

        fn with_send(project: &mut Project) -> manifold_core::AudioSendId {
            let send = manifold_core::audio_setup::AudioSend::new("Kick");
            let id = send.id.clone();
            project.audio_setup.sends.push(send);
            id
        }

        fn test_feature() -> manifold_core::audio_mod::AudioFeature {
            manifold_core::audio_mod::AudioFeature::new(
                manifold_core::audio_mod::AudioFeatureKind::Amplitude,
                manifold_core::audio_mod::AudioBand::Full,
            )
        }

        // ── Settings sliders ─────────────────────────────────────

        fn master_opacity_case(stomp: bool) {
            let project = Project::default();
            let mut h = Harness::new(None);
            let before = project.settings.master_opacity;
            let after = 0.42f32;
            trio_cycle(
                "master_opacity",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Scrub(ValueRef::MasterOpacity, ScrubPhase::Begin), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::MasterOpacity, ScrubPhase::Move(ScrubValue::Scalar(0.6))), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::MasterOpacity, ScrubPhase::Move(ScrubValue::Scalar(after))), p);
                },
                |h, p| h.dispatch(&PanelAction::Scrub(ValueRef::MasterOpacity, ScrubPhase::Commit), p),
                |p| p.settings.master_opacity,
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn master_opacity_clean() {
            master_opacity_case(false);
        }

        #[test]
        fn master_opacity_stomp() {
            master_opacity_case(true);
        }

        fn led_brightness_case(stomp: bool) {
            let project = Project::default();
            let mut h = Harness::new(None);
            let before = project.settings.led_brightness;
            let after = 0.37f32;
            trio_cycle(
                "led_brightness",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Scrub(ValueRef::LedBrightness, ScrubPhase::Begin), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::LedBrightness, ScrubPhase::Move(ScrubValue::Scalar(0.9))), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::LedBrightness, ScrubPhase::Move(ScrubValue::Scalar(after))), p);
                },
                |h, p| h.dispatch(&PanelAction::Scrub(ValueRef::LedBrightness, ScrubPhase::Commit), p),
                |p| p.settings.led_brightness,
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn led_brightness_clean() {
            led_brightness_case(false);
        }

        #[test]
        fn led_brightness_stomp() {
            led_brightness_case(true);
        }

        fn macro_case(stomp: bool) {
            let mut project = Project::default();
            project.settings.macro_bank.slots[0].value = 0.2;
            let mut h = Harness::new(None);
            trio_cycle(
                "macro",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Scrub(ValueRef::Macro(0), ScrubPhase::Begin), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::Macro(0), ScrubPhase::Move(ScrubValue::Scalar(0.5))), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::Macro(0), ScrubPhase::Move(ScrubValue::Scalar(0.8))), p);
                },
                |h, p| h.dispatch(&PanelAction::Scrub(ValueRef::Macro(0), ScrubPhase::Commit), p),
                |p| p.settings.macro_bank.slots[0].value,
                0.2,
                0.8,
                stomp,
            );
        }

        #[test]
        fn macro_clean() {
            macro_case(false);
        }

        #[test]
        fn macro_stomp() {
            macro_case(true);
        }

        // ── Layer sliders ────────────────────────────────────────

        fn layer_opacity_case(stomp: bool) {
            let (project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let lid = layer_id.clone();
            let before = project
                .timeline
                .find_layer_by_id(&layer_id)
                .map(|(_, l)| l.opacity)
                .unwrap();
            trio_cycle(
                "layer_opacity",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Scrub(ValueRef::LayerOpacity, ScrubPhase::Begin), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::LayerOpacity, ScrubPhase::Move(ScrubValue::Scalar(0.9))), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::LayerOpacity, ScrubPhase::Move(ScrubValue::Scalar(0.55))), p);
                },
                |h, p| h.dispatch(&PanelAction::Scrub(ValueRef::LayerOpacity, ScrubPhase::Commit), p),
                move |p| {
                    p.timeline
                        .find_layer_by_id(&lid)
                        .map(|(_, l)| l.opacity)
                        .unwrap()
                },
                before,
                0.55,
                stomp,
            );
        }

        #[test]
        fn layer_opacity_clean() {
            layer_opacity_case(false);
        }

        #[test]
        fn layer_opacity_stomp() {
            layer_opacity_case(true);
        }

        fn audio_gain_case(stomp: bool) {
            let (project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let lid = layer_id.clone();
            let lid2 = layer_id.clone();
            let before = project
                .timeline
                .find_layer_by_id(&layer_id)
                .map(|(_, l)| l.audio_gain_db)
                .unwrap();
            trio_cycle(
                "audio_gain",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Scrub(ValueRef::LayerAudioGain(lid.clone()), ScrubPhase::Begin), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::LayerAudioGain(lid.clone()), ScrubPhase::Move(ScrubValue::Scalar(3.0))), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::LayerAudioGain(lid.clone()), ScrubPhase::Move(ScrubValue::Scalar(-6.0))), p);
                },
                |h, p| h.dispatch(&PanelAction::Scrub(ValueRef::LayerAudioGain(lid.clone()), ScrubPhase::Commit), p),
                move |p| {
                    p.timeline
                        .find_layer_by_id(&lid2)
                        .map(|(_, l)| l.audio_gain_db)
                        .unwrap()
                },
                before,
                -6.0,
                stomp,
            );
        }

        #[test]
        fn audio_gain_clean() {
            audio_gain_case(false);
        }

        #[test]
        fn audio_gain_stomp() {
            audio_gain_case(true);
        }

        // ── Card param drag (exposed manifest slot) ──────────────

        fn card_param_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let before = gen_inst(&project, &layer_id).get_base_param(pid.as_ref());
            let after = before + 0.25;
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            trio_cycle(
                "card_param",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Scrub(ValueRef::Param(gpt(), pid.clone()), ScrubPhase::Begin), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::Param(gpt(), pid.clone()), ScrubPhase::Move(ScrubValue::Scalar(before + 0.1))), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::Param(gpt(), pid.clone()), ScrubPhase::Move(ScrubValue::Scalar(after))), p);
                },
                |h, p| h.dispatch(&PanelAction::Scrub(ValueRef::Param(gpt(), pid.clone()), ScrubPhase::Commit), p),
                move |p| gen_inst(p, &probe_lid).get_base_param(probe_pid.as_ref()),
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn card_param_clean() {
            card_param_case(false);
        }

        #[test]
        fn card_param_stomp() {
            card_param_case(true);
        }

        /// Two SceneStarter generator layers (same structural preset, so a
        /// P1-stamped exposed param id resolves in either instance).
        fn two_scene_layer_project() -> (Project, LayerId, LayerId) {
            let mut project = Project::default();
            let idx_a = project.timeline.add_layer(
                "A",
                LayerType::Generator,
                PresetTypeId::from_string("SceneStarter".to_string()),
            );
            let layer_a = project.timeline.layers[idx_a].layer_id.clone();
            let idx_b = project.timeline.add_layer(
                "B",
                LayerType::Generator,
                PresetTypeId::from_string("SceneStarter".to_string()),
            );
            let layer_b = project.timeline.layers[idx_b].layer_id.clone();
            (project, layer_a, layer_b)
        }

        /// BUG-292: the scene panel's rows dispatch via
        /// `GraphParamTarget::GeneratorOf(<the panel's own bound layer>)`,
        /// never plain `Generator` (which `resolve_graph_target` resolves
        /// through `active_layer`). Panel bound to layer A, app active layer
        /// B — a scene-row write must land on A and leave B untouched, the
        /// exact mismatch the old plain-`Generator` dispatch silently wrote
        /// to the wrong layer under.
        #[test]
        fn bug_292_scene_row_writes_target_the_panels_bound_layer_not_active() {
            let (mut project, layer_a, layer_b) = two_scene_layer_project();
            let mut h = Harness::new(Some(layer_b.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_a);
            let before_a = gen_inst(&project, &layer_a).get_base_param(pid.as_ref());
            let before_b = gen_inst(&project, &layer_b).get_base_param(pid.as_ref());
            let after = before_a + 0.25;

            let target = manifold_ui::GraphParamTarget::GeneratorOf(layer_a.clone());
            h.dispatch(&PanelAction::Scrub(ValueRef::Param(target.clone(), pid.clone()), ScrubPhase::Begin), &mut project);
            h.dispatch(&PanelAction::Scrub(ValueRef::Param(target.clone(), pid.clone()), ScrubPhase::Move(ScrubValue::Scalar(after))), &mut project);
            h.dispatch(&PanelAction::Scrub(ValueRef::Param(target, pid.clone()), ScrubPhase::Commit), &mut project);

            assert_eq!(
                gen_inst(&project, &layer_a).get_base_param(pid.as_ref()),
                after,
                "scene row write must land on the panel's BOUND layer (A), not the active layer (B)"
            );
            assert_eq!(
                gen_inst(&project, &layer_b).get_base_param(pid.as_ref()),
                before_b,
                "the active layer (B) must be untouched by a scene row write bound to layer A"
            );
        }

        // ── Modulation trims + envelope handles ──────────────────

        /// Arm a driver (trim 0..1) on a materialized exposed param — the
        /// materialize dispatch itself arms the driver, so this is one call.
        fn arm_driver(
            h: &mut Harness,
            project: &mut Project,
            layer_id: &LayerId,
        ) -> manifold_core::effects::ParamId {
            materialized_param(h, project, layer_id)
        }

        fn driver_trim_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_driver(&mut h, &mut project, &layer_id);
            let probe_lid = layer_id.clone();
            trio_cycle(
                "driver_trim",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(
                        &PanelAction::Scrub(
                            ValueRef::Trim(manifold_ui::panels::TrimKind::Driver, gpt(), pid.clone()),
                            ScrubPhase::Begin,
                        ),
                        p,
                    );
                    h.dispatch(
                        &PanelAction::Scrub(
                            ValueRef::Trim(manifold_ui::panels::TrimKind::Driver, gpt(), pid.clone()),
                            ScrubPhase::Move(ScrubValue::Range(0.3, 0.9)),
                        ),
                        p,
                    );
                },
                |h, p| {
                    h.dispatch(
                        &PanelAction::Scrub(
                            ValueRef::Trim(manifold_ui::panels::TrimKind::Driver, gpt(), pid.clone()),
                            ScrubPhase::Commit,
                        ),
                        p,
                    )
                },
                move |p| {
                    let d = &gen_inst(p, &probe_lid).drivers.as_ref().unwrap()[0];
                    (d.trim_min, d.trim_max)
                },
                (0.0, 1.0),
                (0.3, 0.9),
                stomp,
            );
        }

        #[test]
        fn driver_trim_clean() {
            driver_trim_case(false);
        }

        #[test]
        fn driver_trim_stomp() {
            driver_trim_case(true);
        }

        /// Arm an envelope on a materialized exposed param.
        fn arm_envelope(
            h: &mut Harness,
            project: &mut Project,
            layer_id: &LayerId,
        ) -> manifold_core::effects::ParamId {
            let pid = materialized_param(h, project, layer_id);
            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            project.with_preset_graph_mut(&target, |inst| {
                inst.envelopes = Some(vec![manifold_core::effects::ParamEnvelope {
                    param_id: pid.clone(),
                    enabled: true,
                    target_normalized: 0.2,
                    decay_beats: 1.0,
                    legacy_param_index: None,
                    current_level: 0.0,
                    was_clip_active: false,
                }]);
            });
            pid
        }

        fn envelope_target_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_envelope(&mut h, &mut project, &layer_id);
            let probe_lid = layer_id.clone();
            trio_cycle(
                "envelope_target",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Modulation(ModulationAction::TargetSnapshot(gpt(), pid.clone())), p);
                    h.dispatch(&PanelAction::Modulation(ModulationAction::TargetChanged(gpt(), pid.clone(), 0.75)), p);
                },
                |h, p| h.dispatch(&PanelAction::Modulation(ModulationAction::TargetCommit(gpt(), pid.clone())), p),
                move |p| gen_inst(p, &probe_lid).envelopes.as_ref().unwrap()[0].target_normalized,
                0.2,
                0.75,
                stomp,
            );
        }

        #[test]
        fn envelope_target_clean() {
            envelope_target_case(false);
        }

        #[test]
        fn envelope_target_stomp() {
            envelope_target_case(true);
        }

        fn envelope_decay_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_envelope(&mut h, &mut project, &layer_id);
            let probe_lid = layer_id.clone();
            trio_cycle(
                "envelope_decay",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Modulation(ModulationAction::EnvDecaySnapshot(gpt(), pid.clone())), p);
                    h.dispatch(&PanelAction::Modulation(ModulationAction::EnvDecayChanged(gpt(), pid.clone(), 3.5)), p);
                },
                |h, p| h.dispatch(&PanelAction::Modulation(ModulationAction::EnvDecayCommit(gpt(), pid.clone())), p),
                move |p| gen_inst(p, &probe_lid).envelopes.as_ref().unwrap()[0].decay_beats,
                1.0,
                3.5,
                stomp,
            );
        }

        #[test]
        fn envelope_decay_clean() {
            envelope_decay_case(false);
        }

        #[test]
        fn envelope_decay_stomp() {
            envelope_decay_case(true);
        }

        // ── Audio modulation drawer sliders ──────────────────────

        fn arm_audio_mod(
            h: &mut Harness,
            project: &mut Project,
            layer_id: &LayerId,
        ) -> manifold_core::effects::ParamId {
            let send_id = with_send(project);
            let pid = materialized_param(h, project, layer_id);
            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            project.with_preset_graph_mut(&target, |inst| {
                inst.audio_mods_mut().push(
                    manifold_core::audio_mod::ParameterAudioMod::new(
                        pid.clone(),
                        send_id,
                        test_feature(),
                    ),
                );
            });
            pid
        }

        fn audio_mod_shape_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_audio_mod(&mut h, &mut project, &layer_id);
            let before = gen_inst(&project, &layer_id)
                .find_audio_mod(pid.as_ref())
                .map(|m| m.shape.sensitivity)
                .unwrap();
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            trio_cycle(
                "audio_mod_shape",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Modulation(ModulationAction::AudioModShapeSnapshot(gpt(), pid.clone())), p);
                    h.dispatch(
                        &PanelAction::Modulation(ModulationAction::AudioModShapeParamChanged(
                            gpt(),
                            pid.clone(),
                            manifold_ui::panels::AudioShapeParam::Sensitivity,
                            0.83,
                        )),
                        p,
                    );
                },
                |h, p| h.dispatch(&PanelAction::Modulation(ModulationAction::AudioModShapeCommit(gpt(), pid.clone())), p),
                move |p| {
                    gen_inst(p, &probe_lid)
                        .find_audio_mod(probe_pid.as_ref())
                        .map(|m| m.shape.sensitivity)
                        .unwrap()
                },
                before,
                0.83,
                stomp,
            );
        }

        #[test]
        fn audio_mod_shape_clean() {
            audio_mod_shape_case(false);
        }

        #[test]
        fn audio_mod_shape_stomp() {
            audio_mod_shape_case(true);
        }

        fn audio_trigger_shape_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let send_id = with_send(&mut project);
            let (_, layer) = project.timeline.find_layer_by_id_mut(&layer_id).unwrap();
            layer.clip_triggers.push(
                manifold_core::audio_trigger::LayerClipTrigger::new(
                    manifold_core::audio_mod::AudioModSource {
                        send_id,
                        feature: test_feature(),
                    },
                ),
            );
            let before = project
                .timeline
                .find_layer_by_id(&layer_id)
                .map(|(_, l)| l.clip_triggers[0].shape.sensitivity)
                .unwrap();
            let mut h = Harness::new(Some(layer_id.clone()));
            let lid = layer_id.clone();
            let lid2 = layer_id.clone();
            let lid3 = layer_id.clone();
            trio_cycle(
                "audio_trigger_shape",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeSnapshot(lid.clone(), 0)), p);
                    h.dispatch(
                        &PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeParamChanged(
                            lid.clone(),
                            0,
                            manifold_ui::panels::AudioShapeParam::Sensitivity,
                            0.91,
                        )),
                        p,
                    );
                },
                move |h, p| h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeCommit(lid2.clone(), 0)), p),
                move |p| {
                    p.timeline
                        .find_layer_by_id(&lid3)
                        .map(|(_, l)| l.clip_triggers[0].shape.sensitivity)
                        .unwrap()
                },
                before,
                0.91,
                stomp,
            );
        }

        #[test]
        fn audio_trigger_shape_clean() {
            audio_trigger_shape_case(false);
        }

        #[test]
        fn audio_trigger_shape_stomp() {
            audio_trigger_shape_case(true);
        }

        // ── Audio Setup panel drags ──────────────────────────────

        fn audio_send_gain_case(stomp: bool) {
            let mut project = Project::default();
            let send_id = with_send(&mut project);
            let before = project.audio_setup.find_send(&send_id).unwrap().gain_db;
            let mut h = Harness::new(None);
            let sid = send_id.clone();
            let sid2 = send_id.clone();
            let sid3 = send_id.clone();
            trio_cycle(
                "audio_send_gain",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioSendGainDragBegin(sid.clone())), p);
                    h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioSendGainDragChanged(sid.clone(), 4.0)), p);
                    h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioSendGainDragChanged(sid.clone(), -3.0)), p);
                },
                move |h, p| h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioSendGainDragCommit(sid2.clone())), p),
                move |p| p.audio_setup.find_send(&sid3).unwrap().gain_db,
                before,
                -3.0,
                stomp,
            );
        }

        #[test]
        fn audio_send_gain_clean() {
            audio_send_gain_case(false);
        }

        #[test]
        fn audio_send_gain_stomp() {
            audio_send_gain_case(true);
        }

        fn audio_crossover_case(stomp: bool) {
            let project = Project::default();
            let before = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
            let after = (before.0, before.1 + 1000.0);
            let mut h = Harness::new(None);
            trio_cycle(
                "audio_crossover",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioCrossoverDragBegin), p);
                    h.dispatch(
                        &PanelAction::AudioSetup(AudioSetupAction::AudioCrossoverChanged(manifold_ui::BandDivider::Mid, after.1)),
                        p,
                    );
                },
                |h, p| h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioCrossoverCommit), p),
                |p| (p.audio_setup.low_hz, p.audio_setup.mid_hz),
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn audio_crossover_clean() {
            audio_crossover_case(false);
        }

        #[test]
        fn audio_crossover_stomp() {
            audio_crossover_case(true);
        }

        // ── Relight knobs ────────────────────────────────────────

        fn relight_param_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let _pid = materialized_param(&mut h, &mut project, &layer_id);
            let field = manifold_ui::panels::UiRelightField::Gain;
            let core_field = crate::ui_translate::relight_field_to_editing(field);
            let before = core_field.get(&gen_inst(&project, &layer_id).relight_params);
            let after = before + 0.5;
            let probe_lid = layer_id.clone();
            trio_cycle(
                "relight_param",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Scrub(ValueRef::RelightParam(gpt(), field), ScrubPhase::Begin), p);
                    h.dispatch(&PanelAction::Scrub(ValueRef::RelightParam(gpt(), field), ScrubPhase::Move(ScrubValue::Scalar(after))), p);
                },
                |h, p| h.dispatch(&PanelAction::Scrub(ValueRef::RelightParam(gpt(), field), ScrubPhase::Commit), p),
                move |p| core_field.get(&gen_inst(p, &probe_lid).relight_params),
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn relight_param_clean() {
            relight_param_case(false);
        }

        #[test]
        fn relight_param_stomp() {
            relight_param_case(true);
        }

        // ── Atomic one-shots (buttons / toggles) ─────────────────

        /// Atomic gesture: dispatch once, feed everything to the content
        /// side, assert the undo cycle.
        fn atomic_cycle<P>(
            label: &str,
            project: Project,
            h: &mut Harness,
            gesture: impl Fn(&mut Harness, &mut Project),
            probe: impl Fn(&Project) -> P,
            before: P,
            after: P,
        ) where
            P: PartialEq + std::fmt::Debug,
        {
            let mut side = ContentSide::new(&project);
            let mut project = project;
            gesture(h, &mut project);
            assert_undo_cycle(&mut side, h.drain(), probe, before, after, label);
        }

        #[test]
        fn param_toggle_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let before = gen_inst(&project, &layer_id).get_base_param(pid.as_ref());
            let after = if before > 0.5 { 0.0 } else { 1.0 };
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "param_toggle",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::Params(ParamsAction::ParamToggle(gpt(), pid.clone())), p);
                },
                move |p| gen_inst(p, &probe_lid).get_base_param(probe_pid.as_ref()),
                before,
                after,
            );
        }

        #[test]
        fn param_fire_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let before = gen_inst(&project, &layer_id).get_base_param(pid.as_ref());
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "param_fire",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::Params(ParamsAction::ParamFire(gpt(), pid.clone())), p);
                },
                move |p| gen_inst(p, &probe_lid).get_base_param(probe_pid.as_ref()),
                before,
                before + 1.0,
            );
        }

        #[test]
        fn driver_toggle_atomic() {
            // The materialize dispatch arms an ENABLED driver; the toggle
            // under test flips it off (one undo unit).
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "driver_toggle_disarm",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::Modulation(ModulationAction::DriverToggle(gpt(), pid.clone())), p);
                },
                move |p| {
                    gen_inst(p, &lid)
                        .drivers
                        .as_ref()
                        .and_then(|ds| ds.iter().find(|d| d.param_id == probe_pid).map(|d| d.enabled))
                        .unwrap_or(true)
                },
                true,
                false,
            );
        }

        #[test]
        fn envelope_toggle_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "envelope_toggle_arm",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::Modulation(ModulationAction::EnvelopeToggle(gpt(), pid.clone())), p);
                },
                move |p| {
                    gen_inst(p, &lid)
                        .envelopes
                        .as_ref()
                        .map(|es| es.iter().filter(|e| e.param_id == probe_pid).count())
                        .unwrap_or(0)
                },
                0usize,
                1usize,
            );
        }

        #[test]
        fn audio_mod_toggle_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            with_send(&mut project);
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "audio_mod_toggle_arm",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::Modulation(ModulationAction::AudioModToggle(gpt(), pid.clone())), p);
                },
                move |p| {
                    gen_inst(p, &lid)
                        .find_audio_mod(probe_pid.as_ref())
                        .map(|m| m.enabled)
                        .unwrap_or(false)
                },
                false,
                true,
            );
        }

        #[test]
        fn audio_trigger_add_then_toggle_then_remove_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            with_send(&mut project);
            let mut h = Harness::new(Some(layer_id.clone()));
            let mut side = ContentSide::new(&project);
            let probe = |p: &Project, lid: &LayerId| {
                p.timeline
                    .find_layer_by_id(lid)
                    .map(|(_, l)| (l.clip_triggers.len(), l.clip_triggers.first().map(|t| t.enabled)))
                    .unwrap()
            };
            // Add: one undo unit, (0, None) → (1, Some(true)) — the clip-trigger
            // drawer redesign lands an ENABLED kick trigger so one click fires.
            h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioTriggerAdd(layer_id.clone())), &mut project);
            let cmds = h.drain();
            assert_undo_cycle(
                &mut side,
                cmds,
                |p| probe(p, &layer_id),
                (0usize, None),
                (1usize, Some(true)),
                "audio_trigger_add",
            );
            // Toggle the fresh (already-enabled) row off: one undo unit,
            // Some(true) → Some(false).
            h.dispatch(
                &PanelAction::AudioSetup(AudioSetupAction::AudioTriggerEnabledToggle(layer_id.clone(), 0)),
                &mut project,
            );
            let cmds = h.drain();
            assert_undo_cycle(
                &mut side,
                cmds,
                |p| probe(p, &layer_id),
                (1usize, Some(true)),
                (1usize, Some(false)),
                "audio_trigger_toggle",
            );
            // Remove: one undo unit, back to (0, None).
            h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioTriggerRemove(layer_id.clone(), 0)), &mut project);
            let cmds = h.drain();
            assert_undo_cycle(
                &mut side,
                cmds,
                |p| probe(p, &layer_id),
                (1usize, Some(false)),
                (0usize, None),
                "audio_trigger_remove",
            );
        }

        #[test]
        fn audio_send_gain_typed_atomic() {
            let mut project = Project::default();
            let send_id = with_send(&mut project);
            let before = project.audio_setup.find_send(&send_id).unwrap().gain_db;
            let mut h = Harness::new(None);
            let sid = send_id.clone();
            let sid2 = send_id.clone();
            atomic_cycle(
                "audio_send_gain_typed",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioSendGainSetTyped(sid.clone(), 7.5)), p);
                },
                move |p| p.audio_setup.find_send(&sid2).unwrap().gain_db,
                before,
                7.5,
            );
        }

        #[test]
        fn audio_send_floor_step_atomic() {
            let mut project = Project::default();
            let send_id = with_send(&mut project);
            let before = project.audio_setup.find_send(&send_id).unwrap().floor_db;
            let mut h = Harness::new(None);
            let sid = send_id.clone();
            let sid2 = send_id.clone();
            atomic_cycle(
                "audio_send_floor_step",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioSetup(AudioSetupAction::AudioSendFloorStep(sid.clone(), 1.0)), p);
                },
                move |p| p.audio_setup.find_send(&sid2).unwrap().floor_db,
                before,
                -100.0,
            );
        }

        // ── Ableton macro trim + step-amount (same stomp class) ──

        fn ableton_macro_trim_case(stomp: bool) {
            let mut project = Project::default();
            project.settings.macro_bank.slots[0].ableton_mapping =
                Some(manifold_core::ableton_mapping::AbletonParamMapping {
                    param_id: std::borrow::Cow::Owned("m0".to_string()),
                    address: manifold_core::ableton_mapping::AbletonMacroAddress {
                        track_id: 0,
                        device_id: 0,
                        param_id: 0,
                        device_identity: manifold_core::ableton_mapping::AbletonDeviceIdentity {
                            device_class_name: "InstrumentGroupDevice".to_string(),
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
                    status: manifold_core::ableton_mapping::AbletonMappingStatus::default(),
                });
            let mut h = Harness::new(None);
            trio_cycle(
                "ableton_macro_trim",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Mapping(MappingAction::AbletonMacroTrimSnapshot(0)), p);
                    h.dispatch(&PanelAction::Mapping(MappingAction::AbletonMacroTrimChanged(0, 0.2, 0.7)), p);
                },
                |h, p| h.dispatch(&PanelAction::Mapping(MappingAction::AbletonMacroTrimCommit(0)), p),
                |p| {
                    let m = p.settings.macro_bank.slots[0].ableton_mapping.as_ref().unwrap();
                    (m.range_min, m.range_max)
                },
                (0.0, 1.0),
                (0.2, 0.7),
                stomp,
            );
        }

        #[test]
        fn ableton_macro_trim_clean() {
            ableton_macro_trim_case(false);
        }

        #[test]
        fn ableton_macro_trim_stomp() {
            ableton_macro_trim_case(true);
        }

        fn audio_mod_step_amount_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_audio_mod(&mut h, &mut project, &layer_id);
            // The Step-amount row only exists while the action is Step.
            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            project.with_preset_graph_mut(&target, |inst| {
                if let Some(m) = inst
                    .audio_mods
                    .as_mut()
                    .and_then(|ms| ms.iter_mut().find(|a| a.param_id == pid))
                {
                    m.action = manifold_core::audio_mod::TriggerAction::Step {
                        amount: 0.1,
                        wrap: manifold_core::audio_mod::WrapMode::Wrap,
                    };
                }
            });
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            let read_amount = move |p: &Project| {
                match gen_inst(p, &probe_lid).find_audio_mod(probe_pid.as_ref()).map(|m| m.action) {
                    Some(manifold_core::audio_mod::TriggerAction::Step { amount, .. }) => amount,
                    _ => f32::NAN,
                }
            };
            trio_cycle(
                "audio_mod_step_amount",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::Modulation(ModulationAction::AudioModStepAmountSnapshot(gpt(), pid.clone())), p);
                    h.dispatch(&PanelAction::Modulation(ModulationAction::AudioModStepAmountChanged(gpt(), pid.clone(), 0.65)), p);
                },
                |h, p| h.dispatch(&PanelAction::Modulation(ModulationAction::AudioModStepAmountCommit(gpt(), pid.clone())), p),
                read_amount,
                0.1,
                0.65,
                stomp,
            );
        }

        #[test]
        fn audio_mod_step_amount_clean() {
            audio_mod_step_amount_case(false);
        }

        #[test]
        fn audio_mod_step_amount_stomp() {
            audio_mod_step_amount_case(true);
        }

        // ── Clip gestures (timeline host path) ───────────────────

        use manifold_ui::timeline_editing_host::TimelineEditingHost;

        /// Owns the pieces `AppEditingHost` borrows, so a test can build the
        /// host, drive a gesture, drop the host, then drain the channel.
        struct ClipRig {
            project: Project,
            tx: crossbeam_channel::Sender<ContentCommand>,
            rx: crossbeam_channel::Receiver<ContentCommand>,
            content_state: crate::content_state::ContentState,
            cursor: manifold_ui::cursors::CursorManager,
            active_layer: Option<LayerId>,
            needs_rebuild: bool,
            needs_structural_sync: bool,
            scroll_dirty: crate::ui_root::ScrollDirty,
            invalidate: Vec<usize>,
            pre_drag: Vec<Box<dyn manifold_editing::command::Command>>,
        }

        impl ClipRig {
            fn new(project: Project) -> Self {
                let (tx, rx) = crossbeam_channel::unbounded();
                Self {
                    project,
                    tx,
                    rx,
                    content_state: crate::content_state::ContentState::default(),
                    cursor: manifold_ui::cursors::CursorManager::default(),
                    active_layer: None,
                    needs_rebuild: false,
                    needs_structural_sync: false,
                    scroll_dirty: crate::ui_root::ScrollDirty::default(),
                    invalidate: Vec::new(),
                    pre_drag: Vec::new(),
                }
            }

            fn host(&mut self) -> crate::editing_host::AppEditingHost<'_> {
                crate::editing_host::AppEditingHost::new(
                    &mut self.project,
                    &self.tx,
                    &self.content_state,
                    &mut self.cursor,
                    &mut self.active_layer,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.scroll_dirty,
                    &mut self.invalidate,
                    &mut self.pre_drag,
                )
            }

            fn drain(&self) -> Vec<ContentCommand> {
                self.rx.try_iter().collect()
            }
        }

        /// One video layer + one clip [4..8] created through the REAL host
        /// path; both the rig's project and the returned content side carry it.
        fn clip_project() -> (ClipRig, ContentSide, manifold_core::ClipId) {
            let mut project = Project::default();
            project.timeline.add_layer(
                "V",
                manifold_core::types::LayerType::Video,
                manifold_core::PresetTypeId::from_string("Video".to_string()),
            );
            let mut rig = ClipRig::new(project);
            let clip_id = rig
                .host()
                .create_clip_at_position(manifold_core::Beats(4.0), 0, manifold_core::Beats(4.0))
                .expect("clip creation resolves");
            // Setup is NOT under test: the content side simply starts from the
            // post-create project with an empty undo history.
            rig.drain();
            let side = ContentSide::new(&rig.project);
            (rig, side, clip_id)
        }

        /// Immutable clip lookup (the timeline's `find_clip_by_id` takes &mut
        /// for its cache; probes only get `&Project`).
        fn find_clip<'p>(p: &'p Project, id: &manifold_core::ClipId) -> Option<&'p manifold_core::clip::TimelineClip> {
            p.timeline
                .layers
                .iter()
                .flat_map(|l| l.clips.iter())
                .find(|c| c.id == *id)
        }

        fn clip_start(p: &Project, id: &manifold_core::ClipId) -> manifold_core::Beats {
            find_clip(p, id).map(|c| c.start_beat).expect("clip resolves")
        }

        fn clip_duration(p: &Project, id: &manifold_core::ClipId) -> manifold_core::Beats {
            find_clip(p, id).map(|c| c.duration_beats).expect("clip resolves")
        }

        fn clip_count(p: &Project) -> usize {
            p.timeline.layers.iter().map(|l| l.clips.len()).sum()
        }

        #[test]
        fn clip_create_atomic() {
            // clip_project's setup IS the create gesture — verify it recorded
            // exactly one undo unit that round-trips. Rebuild it inline so the
            // drain isn't consumed as setup.
            let mut project = Project::default();
            project.timeline.add_layer(
                "V",
                manifold_core::types::LayerType::Video,
                manifold_core::PresetTypeId::from_string("Video".to_string()),
            );
            let mut rig = ClipRig::new(project);
            let mut side = ContentSide::new(&rig.project);
            let id = rig
                .host()
                .create_clip_at_position(manifold_core::Beats(4.0), 0, manifold_core::Beats(4.0));
            assert!(id.is_some(), "create resolves a clip id");
            assert_undo_cycle(
                &mut side,
                rig.drain(),
                clip_count,
                0usize,
                1usize,
                "clip_create",
            );
        }

        #[test]
        fn clip_move_atomic() {
            let (mut rig, mut side, clip_id) = clip_project();
            {
                let mut host = rig.host();
                host.begin_command_batch();
                host.set_clip_start_beat(clip_id.as_str(), manifold_core::Beats(12.0));
                host.record_move(clip_id.as_str(), manifold_core::Beats(4.0), manifold_core::Beats(12.0), 0, 0);
                host.commit_command_batch("Move Clip");
            }
            let pid = clip_id.clone();
            assert_undo_cycle(
                &mut side,
                rig.drain(),
                move |p| clip_start(p, &pid),
                manifold_core::Beats(4.0),
                manifold_core::Beats(12.0),
                "clip_move",
            );
        }

        #[test]
        fn clip_trim_atomic() {
            let (mut rig, mut side, clip_id) = clip_project();
            let old_dur = clip_duration(&rig.project, &clip_id);
            {
                let mut host = rig.host();
                host.begin_command_batch();
                host.set_clip_trim(
                    clip_id.as_str(),
                    manifold_core::Beats(4.0),
                    manifold_core::Beats(2.0),
                    manifold_core::Seconds(0.0),
                );
                host.record_trim(
                    clip_id.as_str(),
                    manifold_core::Beats(4.0),
                    manifold_core::Beats(4.0),
                    old_dur,
                    manifold_core::Beats(2.0),
                    manifold_core::Seconds(0.0),
                    manifold_core::Seconds(0.0),
                );
                host.commit_command_batch("Trim Clip");
            }
            let pid = clip_id.clone();
            assert_undo_cycle(
                &mut side,
                rig.drain(),
                move |p| clip_duration(p, &pid),
                old_dur,
                manifold_core::Beats(2.0),
                "clip_trim",
            );
        }

        #[test]
        fn clip_delete_atomic() {
            let (mut rig, mut side, clip_id) = clip_project();
            let mut ui = UIRoot::new();
            let mut selection = manifold_ui::UIState::new();
            let mut active_layer = None;
            let mut prefs = crate::user_prefs::UserPrefs::for_test();
            crate::ui_bridge::editing::dispatch_editing(
                &EditingAction::ContextDeleteClip(clip_id.to_string()),
                &mut rig.project,
                &rig.tx,
                &rig.content_state,
                &mut ui,
                &mut selection,
                &mut active_layer,
                &mut prefs,
            );
            assert_undo_cycle(
                &mut side,
                rig.drain(),
                clip_count,
                1usize,
                0usize,
                "clip_delete",
            );
        }

        /// WIDGET_TREE_DESIGN.md §7 P4, gaps #2/#3 carried from
        /// `docs/landings/2026-07-21-widget-tree-p2.md`: bridge-level
        /// dispatch tests for the modulation-family action kinds — every
        /// test above this point dispatches against a GENERATOR target
        /// only. Each kind here dispatches the SAME `PanelAction` against
        /// BOTH a master-effect `GraphTarget` and a layer-effect
        /// `GraphTarget`, through the identical generic path
        /// (`resolve_mod_target`/`resolve_graph_target` →
        /// `with_preset_graph_mut`/`DriverTarget::from`/`Project::
        /// find_effect_by_id`) — the "fixed for Master, forgot Layer" class
        /// detector the twin consolidation (D2) exists to make impossible.
        ///
        /// Master and layer targets are reached via `Harness::
        /// dispatch_with_editor`'s `editor_target = Some(GraphTarget::
        /// Effect(id))` — the graph editor's own identity-addressed entry
        /// point (`resolve_effect_id`/`editor_dispatch_context`, `ui_bridge/
        /// mod.rs`). That is the only production path that can select a
        /// SPECIFIC effect instance from a bridge-level test: the ambient
        /// route (`editor_target: None`) resolves positionally through
        /// `ui.inspector.last_effect_tab()`, a field only a real
        /// pointer-down sets (`InspectorPanel::update_last_effect_tab`,
        /// private to `manifold-ui`) — driving that is P2's own click-
        /// routing test surface, not this bridge's.
        mod row_dispatch {
            use super::*;

            /// A real effect instance — `init_defaults()` populates `params`
            /// from the registry the same way a live Add Effect does — plus
            /// its first manifest param id.
            fn effect_with_first_param(
                effect_type: manifold_core::PresetTypeId,
            ) -> (PresetInstance, manifold_core::effects::ParamId) {
                let mut fx = PresetInstance::new(effect_type);
                fx.init_defaults();
                let pid = manifold_core::preset_definition_registry::try_get(fx.effect_type())
                    .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
                    .expect("preset has at least one manifest param");
                (fx, std::borrow::Cow::Owned(pid))
            }

            /// One project carrying the SAME preset type as both a master
            /// effect and a layer effect, so a test can dispatch the
            /// identical action against either `GraphTarget` and compare.
            struct TwoScopes {
                project: Project,
                master_target: manifold_core::GraphTarget,
                layer_target: manifold_core::GraphTarget,
                pid: manifold_core::effects::ParamId,
            }

            fn two_scopes(effect_type: &'static str) -> TwoScopes {
                let et = manifold_core::PresetTypeId::new(effect_type);
                let (master_fx, pid) = effect_with_first_param(et.clone());
                let master_id = master_fx.id.clone();
                let (layer_fx, _) = effect_with_first_param(et);
                let layer_effect_id = layer_fx.id.clone();

                let (mut project, layer_id) = scene_layer_project();
                project.settings.master_effects.push(master_fx);
                project
                    .timeline
                    .find_layer_by_id_mut(&layer_id)
                    .expect("fixture layer resolves")
                    .1
                    .effects_mut()
                    .push(layer_fx);

                TwoScopes {
                    project,
                    master_target: manifold_core::GraphTarget::Effect(master_id),
                    layer_target: manifold_core::GraphTarget::Effect(layer_effect_id),
                    pid,
                }
            }

            fn arm_driver(
                project: &mut Project,
                target: &manifold_core::GraphTarget,
                param_id: &manifold_core::effects::ParamId,
            ) {
                project.with_preset_graph_mut(target, |inst| {
                    inst.drivers = Some(vec![ParameterDriver {
                        param_id: param_id.clone(),
                        beat_division: BeatDivision::Quarter,
                        waveform: DriverWaveform::Sine,
                        enabled: true,
                        phase: 0.0,
                        base_value: 0.0,
                        trim_min: 0.0,
                        trim_max: 1.0,
                        reversed: false,
                        free_period_beats: None,
                        legacy_param_index: None,
                        is_paused_by_user: false,
                    }]);
                });
            }

            fn arm_ableton_mapping(
                project: &mut Project,
                target: &manifold_core::GraphTarget,
                param_id: &manifold_core::effects::ParamId,
            ) {
                project.with_preset_graph_mut(target, |inst| {
                    inst.ableton_mappings = Some(vec![manifold_core::ableton_mapping::AbletonParamMapping {
                        param_id: param_id.clone(),
                        address: manifold_core::ableton_mapping::AbletonMacroAddress {
                            track_id: 0,
                            device_id: 0,
                            param_id: 0,
                            device_identity: manifold_core::ableton_mapping::AbletonDeviceIdentity {
                                device_class_name: "InstrumentGroupDevice".to_string(),
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
                        status: manifold_core::ableton_mapping::AbletonMappingStatus::default(),
                    }]);
                });
            }

            /// Dispatch `action` against `target` via `editor_target`, drain
            /// into a fresh `ContentSide`, and assert the atomic-gesture
            /// undo-cycle shape `atomic_cycle` proves for the generator
            /// target — replicated per scope target here.
            fn scope_atomic<P>(
                label: &str,
                project: Project,
                target: &manifold_core::GraphTarget,
                action: PanelAction,
                probe: impl Fn(&Project) -> P,
                before: P,
                after: P,
            ) where
                P: PartialEq + std::fmt::Debug,
            {
                let mut side = ContentSide::new(&project);
                let mut project = project;
                let mut h = Harness::new(None);
                h.dispatch_with_editor(&action, &mut project, Some(target));
                assert_undo_cycle(&mut side, h.drain(), probe, before, after, label);
            }

            /// The gesture must produce NO commands at all (dispatch is a
            /// documented no-op for this scope) and leave `probe` unchanged.
            fn scope_inert<P>(
                label: &str,
                mut project: Project,
                target: &manifold_core::GraphTarget,
                action: PanelAction,
                probe: impl Fn(&Project) -> P,
            ) where
                P: PartialEq + std::fmt::Debug,
            {
                let before = probe(&project);
                let mut h = Harness::new(None);
                h.dispatch_with_editor(&action, &mut project, Some(target));
                let cmds = h.drain();
                assert!(
                    cmds.is_empty(),
                    "{label}: must be a documented no-op for this scope; got {} commands",
                    cmds.len()
                );
                assert_eq!(probe(&project), before, "{label}: state must not move");
            }

            // ── DriverToggle ──────────────────────────────────────────

            #[test]
            fn driver_toggle_master() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                scope_atomic(
                    "driver_toggle_master",
                    s.project,
                    &s.master_target,
                    PanelAction::Modulation(ModulationAction::DriverToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.drivers.as_ref())
                            .and_then(|ds| ds.iter().find(|d| d.param_id == pid).map(|d| d.enabled))
                    },
                    None,
                    Some(true),
                );
            }

            #[test]
            fn driver_toggle_layer() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                scope_atomic(
                    "driver_toggle_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::Modulation(ModulationAction::DriverToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.drivers.as_ref())
                            .and_then(|ds| ds.iter().find(|d| d.param_id == pid).map(|d| d.enabled))
                    },
                    None,
                    Some(true),
                );
            }

            // ── AudioModToggle ────────────────────────────────────────

            #[test]
            fn audio_mod_toggle_master() {
                let mut s = two_scopes("Bloom");
                with_send(&mut s.project);
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                scope_atomic(
                    "audio_mod_toggle_master",
                    s.project,
                    &s.master_target,
                    PanelAction::Modulation(ModulationAction::AudioModToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| p.preset_instance(&t).and_then(|inst| inst.find_audio_mod(pid.as_ref())).map(|m| m.enabled),
                    None,
                    Some(true),
                );
            }

            #[test]
            fn audio_mod_toggle_layer() {
                let mut s = two_scopes("Bloom");
                with_send(&mut s.project);
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                scope_atomic(
                    "audio_mod_toggle_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::Modulation(ModulationAction::AudioModToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| p.preset_instance(&t).and_then(|inst| inst.find_audio_mod(pid.as_ref())).map(|m| m.enabled),
                    None,
                    Some(true),
                );
            }

            // ── EnvelopeToggle — layer arms; master is a documented
            //    no-op ("effects are clip-timed", inspector.rs's own
            //    comment on the handler) ─────────────────────────────

            #[test]
            fn envelope_toggle_layer_arms() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                scope_atomic(
                    "envelope_toggle_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::Modulation(ModulationAction::EnvelopeToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.envelopes.as_ref())
                            .map(|es| es.iter().filter(|e| e.param_id == pid).count())
                            .unwrap_or(0)
                    },
                    0usize,
                    1usize,
                );
            }

            #[test]
            fn envelope_toggle_master_is_inert() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                scope_inert(
                    "envelope_toggle_master",
                    s.project,
                    &s.master_target,
                    PanelAction::Modulation(ModulationAction::EnvelopeToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.envelopes.as_ref())
                            .map(|es| es.iter().filter(|e| e.param_id == pid).count())
                            .unwrap_or(0)
                    },
                );
            }

            // ── ParamToggle / ParamFire ───────────────────────────────

            #[test]
            fn param_toggle_master() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                let before = s.project.preset_instance(&s.master_target).unwrap().get_base_param(pid.as_ref());
                let after = if before > 0.5 { 0.0 } else { 1.0 };
                scope_atomic(
                    "param_toggle_master",
                    s.project,
                    &s.master_target,
                    PanelAction::Params(ParamsAction::ParamToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| p.preset_instance(&t).unwrap().get_base_param(pid.as_ref()),
                    before,
                    after,
                );
            }

            #[test]
            fn param_toggle_layer() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                let before = s.project.preset_instance(&s.layer_target).unwrap().get_base_param(pid.as_ref());
                let after = if before > 0.5 { 0.0 } else { 1.0 };
                scope_atomic(
                    "param_toggle_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::Params(ParamsAction::ParamToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| p.preset_instance(&t).unwrap().get_base_param(pid.as_ref()),
                    before,
                    after,
                );
            }

            #[test]
            fn param_fire_master() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                let before = s.project.preset_instance(&s.master_target).unwrap().get_base_param(pid.as_ref());
                scope_atomic(
                    "param_fire_master",
                    s.project,
                    &s.master_target,
                    PanelAction::Params(ParamsAction::ParamFire(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| p.preset_instance(&t).unwrap().get_base_param(pid.as_ref()),
                    before,
                    before + 1.0,
                );
            }

            #[test]
            fn param_fire_layer() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                let before = s.project.preset_instance(&s.layer_target).unwrap().get_base_param(pid.as_ref());
                scope_atomic(
                    "param_fire_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::Params(ParamsAction::ParamFire(manifold_ui::GraphParamTarget::Effect(0), pid.clone())),
                    move |p| p.preset_instance(&t).unwrap().get_base_param(pid.as_ref()),
                    before,
                    before + 1.0,
                );
            }

            // ── DriverConfig (one representative: a BeatDiv click) ────

            #[test]
            fn driver_config_beat_div_master() {
                let mut s = two_scopes("Bloom");
                arm_driver(&mut s.project, &s.master_target, &s.pid);
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                scope_atomic(
                    "driver_config_beat_div_master",
                    s.project,
                    &s.master_target,
                    PanelAction::Modulation(ModulationAction::DriverConfig(
                        manifold_ui::GraphParamTarget::Effect(0),
                        pid.clone(),
                        DriverConfigAction::BeatDiv(4), // -> Half
                    )),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.drivers.as_ref())
                            .and_then(|ds| ds.iter().find(|d| d.param_id == pid).map(|d| d.beat_division))
                    },
                    Some(BeatDivision::Quarter),
                    Some(BeatDivision::Half),
                );
            }

            #[test]
            fn driver_config_beat_div_layer() {
                let mut s = two_scopes("Bloom");
                arm_driver(&mut s.project, &s.layer_target, &s.pid);
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                scope_atomic(
                    "driver_config_beat_div_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::Modulation(ModulationAction::DriverConfig(
                        manifold_ui::GraphParamTarget::Effect(0),
                        pid.clone(),
                        DriverConfigAction::BeatDiv(4), // -> Half
                    )),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.drivers.as_ref())
                            .and_then(|ds| ds.iter().find(|d| d.param_id == pid).map(|d| d.beat_division))
                    },
                    Some(BeatDivision::Quarter),
                    Some(BeatDivision::Half),
                );
            }

            // ── AbletonInvertToggle — NOT undo-tracked (mirrors
            //    `TrimChanged`'s Ableton branch: a `MutateProject` write,
            //    no `Execute`) — assert the flip lands identically on both
            //    scopes without asserting a false undo requirement ───────

            fn ableton_invert_case(label: &str, mut project: Project, target: &manifold_core::GraphTarget, pid: manifold_core::effects::ParamId) {
                let t = target.clone();
                let p2 = pid.clone();
                let probe = move |proj: &Project| -> Option<bool> {
                    proj.preset_instance(&t)
                        .and_then(|inst| inst.ableton_mappings.as_ref())
                        .and_then(|ms| ms.iter().find(|m| m.param_id == p2).map(|m| m.inverted))
                };
                let before = probe(&project).expect("fixture must start with a mapping present");
                assert!(!before, "fixture starts uninverted");

                let mut side = ContentSide::new(&project);
                let mut h = Harness::new(None);
                h.dispatch_with_editor(
                    &PanelAction::Mapping(MappingAction::AbletonInvertToggle(manifold_ui::GraphParamTarget::Effect(0), pid)),
                    &mut project,
                    Some(target),
                );
                assert_eq!(probe(&project), Some(true), "{label}: local project must flip");

                let cmds = h.drain();
                assert!(
                    !cmds.iter().any(|c| matches!(c, ContentCommand::Execute(_))),
                    "{label}: Ableton mapping edits are deliberately not undo-tracked"
                );
                let landed = side.apply(cmds);
                assert_eq!(landed, 0, "{label}: no undo-tracked command should land");
                assert_eq!(probe(&side.project), Some(true), "{label}: content mirror must flip too");
            }

            #[test]
            fn ableton_invert_toggle_master() {
                let mut s = two_scopes("Bloom");
                arm_ableton_mapping(&mut s.project, &s.master_target, &s.pid);
                let pid = s.pid.clone();
                ableton_invert_case("ableton_invert_master", s.project, &s.master_target, pid);
            }

            #[test]
            fn ableton_invert_toggle_layer() {
                let mut s = two_scopes("Bloom");
                arm_ableton_mapping(&mut s.project, &s.layer_target, &s.pid);
                let pid = s.pid.clone();
                ableton_invert_case("ableton_invert_layer", s.project, &s.layer_target, pid);
            }
        }
    }

    /// BUG-262 regression. The mapping-sidebar range/affine drags dispatch
    /// through `app_render`'s `pending_actions` loop, not the inspector host
    /// the matrix above drives, so they can't ride `trio_cycle`. What made
    /// them lose undo entries was a mid-gesture full-snapshot *stomp*
    /// reverting the in-flight reshape before the commit read it back via
    /// `watched_reshape` — the exact failure `ActiveInspectorDrag::apply` now
    /// prevents. These prove the restore at that level: given the guard a live
    /// drag installs, a stale pre-drag snapshot must come back carrying the
    /// dragged value, so the commit sees new != old and records one undo.
    mod mapping_undo_baseline {
        use super::*;

        /// A master effect carrying one user param binding whose reshape lives
        /// in the per-instance graph (mirrors the editing crate's
        /// `project_with_one_user_binding`). Pre-drag range 0..1, affine 1/0.
        fn project_with_binding() -> (Project, manifold_core::GraphTarget, String) {
            let mut project = Project::default();
            let mut fx = manifold_core::effects::PresetInstance::new(
                manifold_core::PresetTypeId::new("Mirror"),
            );
            let effect_id = fx.id.clone();
            let binding_id = "user.uv_transform.rotation.1".to_string();
            fx.append_user_binding(manifold_core::effects::UserParamBinding {
                id: binding_id.clone(),
                label: "Original Label".to_string(),
                node_id: manifold_core::NodeId::new("uv_transform"),
                legacy_node_handle: None,
                inner_param: "rotation".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 0.25,
                convert: manifold_core::effects::ParamConvert::Float,
                is_angle: false,
                invert: false,
                curve: manifold_core::macro_bank::MacroCurve::Linear,
                scale: 1.0,
                offset: 0.0,
                value_labels: Vec::new(),
                section: None,
            });
            project.settings.master_effects.push(fx);
            (
                project,
                manifold_core::GraphTarget::Effect(effect_id),
                binding_id,
            )
        }

        /// Read the binding's live `(min, max, scale, offset)` back the way
        /// `watched_reshape` does — through the synthesized binding view.
        fn reshape(project: &Project, id: &str) -> (f32, f32, f32, f32) {
            let b = project.settings.master_effects[0]
                .user_param_bindings()
                .into_iter()
                .find(|b| b.id == id)
                .expect("binding present");
            (b.min, b.max, b.scale, b.offset)
        }

        #[test]
        fn mapping_range_drag_survives_snapshot_stomp() {
            let (project, target, binding_id) = project_with_binding();
            let (min0, max0, _, _) = reshape(&project, &binding_id);
            assert_eq!((min0, max0), (0.0, 1.0), "fixture starts at the default range");

            // The guard a live range drag installs (in-flight range 0.2..0.8).
            let guard = crate::app::ActiveInspectorDrag::MappingRange {
                target,
                param_id: binding_id.clone(),
                min: 0.2,
                max: 0.8,
            };
            // A full snapshot lands mid-drag carrying the stale pre-drag
            // project; app_render restores the guarded drag onto it.
            let mut stomped = project.clone();
            guard.apply(&mut stomped);

            let (min, max, _, _) = reshape(&stomped, &binding_id);
            assert_eq!(
                (min, max),
                (0.2, 0.8),
                "range stomp must be undone so the commit sees new != old and records undo"
            );
        }

        #[test]
        fn mapping_affine_drag_survives_snapshot_stomp() {
            let (project, target, binding_id) = project_with_binding();
            let (_, _, scale0, offset0) = reshape(&project, &binding_id);
            assert_eq!((scale0, offset0), (1.0, 0.0), "fixture starts at identity affine");

            let guard = crate::app::ActiveInspectorDrag::MappingAffine {
                target,
                param_id: binding_id.clone(),
                scale: 2.5,
                offset: -0.5,
            };
            let mut stomped = project.clone();
            guard.apply(&mut stomped);

            let (_, _, scale, offset) = reshape(&stomped, &binding_id);
            assert_eq!(
                (scale, offset),
                (2.5, -0.5),
                "affine stomp must be undone so the commit sees new != old and records undo"
            );
        }
    }

    /// BUG-266: the inspector tab pin was clearing on ANY `selection_version`
    /// bump, including ones from command side effects (add-effect's
    /// behind-the-scenes selection touch) that never change WHICH thing is
    /// selected. Three probes on the real path (`state_sync::
    /// sync_inspector_data`, the same fn the app's per-frame sync calls):
    /// a version bump with unchanged selection identity must not clear the
    /// pin; a genuine identity change must; a transient empty selection
    /// (clear-then-reselect churn) must not.
    mod bug_266_tab_pin {
        use super::*;
        use manifold_ui::InspectorTab;

        fn active_tab(
            ui: &mut UIRoot,
            project: &Project,
            active_layer: Option<usize>,
            selection: &SelectionState,
        ) -> InspectorTab {
            crate::ui_bridge::state_sync::sync_inspector_data(
                ui,
                project,
                active_layer,
                selection,
                &[],
            );
            ui.inspector.active_tab()
        }

        #[test]
        fn incidental_version_bump_does_not_clear_the_pin() {
            let (project, layer_id) = scene_layer_project();
            let idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
            let mut ui = UIRoot::new();
            let mut selection = SelectionState::new();
            selection.select_layer(layer_id.clone());
            selection.pin_scope(InspectorTab::Master);
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx), &selection),
                InspectorTab::Master
            );

            // What add-effect's behind-the-scenes selection touch looks like
            // at the ui_state level: re-selecting the SAME layer bumps
            // `selection_version` without changing WHICH layer is selected.
            let before = selection.selection_version;
            selection.select_layer(layer_id.clone());
            assert!(
                selection.selection_version > before,
                "sanity: version must actually bump"
            );
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx), &selection),
                InspectorTab::Master,
                "pin must survive a version bump that doesn't change WHICH layer is selected"
            );
        }

        #[test]
        fn genuine_selection_change_clears_the_pin() {
            let (mut project, layer_id) = scene_layer_project();
            let idx2 = project.timeline.add_layer(
                "Scene2",
                LayerType::Generator,
                PresetTypeId::from_string("SceneStarter".to_string()),
            );
            let layer_id_2 = project.timeline.layers[idx2].layer_id.clone();
            let idx1 = project.timeline.find_layer_index_by_id(&layer_id).unwrap();

            let mut ui = UIRoot::new();
            let mut selection = SelectionState::new();
            selection.select_layer(layer_id.clone());
            selection.pin_scope(InspectorTab::Master);
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx1), &selection),
                InspectorTab::Master
            );

            selection.select_layer(layer_id_2.clone());
            let idx2 = project.timeline.find_layer_index_by_id(&layer_id_2).unwrap();
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx2), &selection),
                InspectorTab::Layer,
                "a genuine selection change (different layer) must drop the pin back to \
                 the selection-derived default"
            );
        }

        #[test]
        fn transient_empty_selection_holds_the_pin() {
            let (project, layer_id) = scene_layer_project();
            let idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
            let mut ui = UIRoot::new();
            let mut selection = SelectionState::new();
            selection.select_layer(layer_id.clone());
            selection.pin_scope(InspectorTab::Master);
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx), &selection),
                InspectorTab::Master
            );

            // Clear-then-reselect churn: an empty selection observed
            // mid-gesture must not itself kill the pin.
            selection.clear_layer_selection();
            assert_eq!(
                active_tab(&mut ui, &project, None, &selection),
                InspectorTab::Master,
                "a transient empty selection must not clear the pin"
            );

            // ...and reselecting the SAME layer afterward still finds it pinned.
            selection.select_layer(layer_id.clone());
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx), &selection),
                InspectorTab::Master
            );
        }
    }
}
