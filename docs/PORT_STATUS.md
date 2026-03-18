# MANIFOLD Rust Port — File Parity Tracker

**Last updated: 2026-03-17**

Status key: `ported` = substantial Rust implementation exists | `partial` = exists but missing significant parts | `stub` = placeholder only | `missing` = no Rust equivalent

Agents: CHECK THIS FILE before porting. If a file is already `ported`, verify parity instead of re-porting.

---

## Data Models (manifold-core)

| Unity File | Rust File | Status |
|---|---|---|
| BeatQuantizer.cs | math.rs | ported |
| BlendMode.cs | effects.rs (inline) | ported |
| ClipDurationMode.cs | types.rs | ported |
| EffectCategoryRegistry.cs | effects.rs | ported |
| EffectDefinitionRegistry.cs | effects.rs | ported |
| EffectGroup.cs | effects.rs | ported |
| EffectInstance.cs | effects.rs | ported |
| EffectType.cs | types.rs | ported |
| GeneratorDefinitionRegistry.cs | generator.rs | ported |
| GeneratorParamSource.cs | generator.rs | ported |
| GeneratorParamState.cs | generator.rs | ported |
| GeneratorType.cs | types.rs | ported |
| IEffectContainer.cs | effects.rs (trait) | ported |
| IParamSource.cs | generator.rs (trait) | ported |
| Layer.cs | layer.rs | ported |
| LayerType.cs | types.rs | ported |
| MathUtils.cs | math.rs | ported |
| MidiMappingConfig.cs | types.rs | ported |
| MidiNoteParser.cs | midi.rs | ported |
| ParamDef.cs | types.rs | ported |
| ParamEnvelope.cs | generator.rs | ported |
| ParameterDriver.cs | generator.rs | ported |
| PathResolver.cs | — | missing |
| PercussionImportState.cs | percussion.rs | ported |
| Project.cs | project.rs | ported |
| ProjectSettings.cs | settings.rs | ported |
| RecordingProvenance.cs | recording.rs | ported |
| SelectionRegion.cs | selection.rs | ported |
| TempoMap.cs | tempo.rs | ported |
| TempoMapConverter.cs | tempo.rs | ported |
| Timeline.cs | timeline.rs | ported |
| TimelineClip.cs | clip.rs | ported |
| VideoClip.cs | video.rs | ported |
| VideoLibrary.cs | video.rs | partial |

## Editing System (manifold-editing)

| Unity File | Rust File | Status |
|---|---|---|
| ICommand.cs | command.rs | ported |
| UndoRedoManager.cs | undo.rs | ported |
| EditingService.cs | service.rs | ported |
| ClipCommands.cs | commands/clip.rs | ported |
| LayerGroupCommands.cs | commands/layer.rs | ported |
| EffectGroupCommands.cs | commands/effect_groups.rs | ported |
| SettingsCommands.cs | commands/settings.rs | ported |
| EffectClipboard.cs | clipboard.rs | ported |
| ILayerLifecycleCallbacks.cs | — | missing |

## Playback Core (manifold-playback)

| Unity File | Rust File | Status |
|---|---|---|
| PlaybackEngine.cs | engine.rs | ported |
| PlaybackController.cs | engine.rs | ported |
| ClipScheduler.cs | scheduler.rs | ported |
| ActiveTimelineClipWindow.cs | active_window.rs | ported |
| LiveClipManager.cs | live_clip_manager.rs | ported |
| IClipRenderer.cs | renderer.rs (trait) | ported |
| ILiveClipHost.cs | engine.rs (trait) | ported |
| IPlaybackNotifier.cs | engine.rs (trait) | ported |
| ISyncTarget.cs | sync_source.rs | ported |
| GeneratorRenderer.cs | renderer.rs | ported |
| VideoPlayerPool.cs | — | missing (platform-specific) |
| VideoTimeCalculator.cs | video_time.rs | ported |
| TransportController.cs | transport_controller.rs | ported |
| DriverController.cs | modulation.rs (evaluate_modulation) | ported |
| EnvelopeEvaluator.cs | modulation.rs (evaluate_all_envelopes, calculate_adsr) | ported |
| ParameterDriverManager.cs | modulation.rs (evaluate_all_drivers) | ported |
| PerfLogger.cs | — | partial (log crate) |
| TempoRecorder.cs | — | missing (diagnostics) |

## Sync System (manifold-playback)

| Unity File | Rust File | Status |
|---|---|---|
| ISyncSource.cs | sync_source.rs | ported |
| SyncArbiter.cs | sync.rs | ported |
| LinkSyncController.cs | link_sync.rs | ported |
| MidiClockSyncController.cs | midi_clock_sync.rs | ported |
| OscSyncController.cs | sync.rs | partial |
| OscReceiver.cs | sync.rs | partial |
| OscPositionSender.cs | osc_sender.rs | partial |
| OscParameterRegistry.cs | osc_sender.rs | ported |
| MasterEffectOscBridge.cs | osc_sender.rs | partial |
| LayerEffectOscBridge.cs | osc_sender.rs | partial |
| LayerOscBridge.cs | osc_sender.rs | partial |
| GeneratorOscBridge.cs | osc_sender.rs | partial |
| AbletonLink.cs | link_sync.rs | ported |
| MidiClock.cs | midi_clock_sync.rs | ported |

## Generators (manifold-renderer/generators) — ALL PORTED

| Unity File | Rust File | Status |
|---|---|---|
| IGenerator.cs | generator.rs (trait) | ported |
| ShaderGeneratorBase.cs | stateful_base.rs | ported |
| LineGeneratorBase.cs | line_pipeline.rs | ported |
| StatefulShaderGeneratorBase.cs | stateful_base.rs | ported |
| ComputeVolumeGeneratorBase.cs | stateful_base.rs | ported |
| ComputeParticleGeneratorBase.cs | stateful_base.rs | ported |
| GeneratorContext.cs | generator_context.rs | ported |
| GeneratorMath.cs | generator_math.rs | ported |
| GeneratorProcessorRegistry.cs | registry.rs | ported |
| LineMeshUtil.cs | line_pipeline.rs | ported |
| BasicShapesSnapGenerator.cs | basic_shapes_snap.rs | ported |
| ComputeParametricSurfaceGenerator.cs | parametric_surface.rs | ported |
| ComputeStrangeAttractorGenerator.cs | compute_strange_attractor.rs | ported |
| ConcentricTunnelGenerator.cs | concentric_tunnel.rs | ported |
| DuocylinderGenerator.cs | duocylinder.rs | ported |
| FlowfieldGenerator.cs | flowfield.rs | ported |
| FluidSimulation3DGenerator.cs | fluid_simulation_3d.rs | ported |
| FluidSimulationGenerator.cs | fluid_simulation.rs | ported |
| FractalZoomGenerator.cs | fractal_zoom.rs | ported |
| LissajousGenerator.cs | lissajous.rs | ported |
| MyceliumGenerator.cs | mycelium.rs | ported |
| NumberStationGenerator.cs | number_station.rs | ported |
| OscilloscopeXYGenerator.cs | oscilloscope_xy.rs | ported |
| PlasmaGenerator.cs | plasma.rs | ported |
| ReactionDiffusionGenerator.cs | reaction_diffusion.rs | ported |
| StrangeAttractorGenerator.cs | strange_attractor.rs | ported |
| TesseractGenerator.cs | tesseract.rs | ported |
| WireframeZooGenerator.cs | wireframe_zoo.rs | ported |

## Compositing Effects (manifold-renderer/effects)

| Unity File | Rust File | Status |
|---|---|---|
| IPostProcessEffect.cs | effect.rs (trait) | ported |
| IStatefulEffect.cs | effect.rs (trait) | ported |
| IEffectHost.cs | effect.rs (trait) | ported |
| SimpleBlitEffect.cs | simple_blit_helper.rs | ported |
| EffectContext.cs | effect.rs | ported |
| EffectProcessorRegistry.cs | effect_registry.rs | ported |
| RenderTextureUtil.cs | render_target.rs | ported |
| BloomFX.cs | bloom.rs | ported |
| ChromaticAberrationFX.cs | chromatic_aberration.rs | ported |
| ColorGradeFX.cs | color_grade.rs | ported |
| CrtFX.cs | crt.rs | ported |
| DitherFX.cs | dither.rs | ported |
| EdgeStretchFX.cs | edge_stretch.rs | ported |
| FeedbackFX.cs | feedback.rs | ported |
| FilmGrainFX.cs | film_grain.rs | ported |
| GlitchFX.cs | glitch.rs | ported |
| HalationFX.cs | halation.rs | ported |
| InvertColorsFX.cs | invert_colors.rs | ported |
| KaleidoscopeFX.cs | kaleidoscope.rs | ported |
| MirrorFX.cs | mirror.rs | ported |
| QuadMirrorFX.cs | quad_mirror.rs | ported |
| StrobeFX.cs | strobe.rs | ported |
| StylizedFeedbackFX.cs | stylized_feedback.rs | ported |
| BlobTrackingFX.cs | effects/blob_tracking.rs | ported |
| ComputeFluidDistortionFX.cs | — | missing |
| ComputePixelSortFX.cs | — | missing |
| CorruptionFX.cs | — | missing |
| DatamoshFX.cs | — | missing |
| EdgeGlowFX.cs | — | missing |
| FluidDistortionFX.cs | — | missing |
| InfiniteZoomFX.cs | — | missing |
| InfraredFX.cs | — | missing |
| MicroscopeFX.cs | — | missing |
| RedactionFX.cs | — | missing |
| SlitScanFX.cs | — | missing |
| SurveillanceFX.cs | — | missing |
| TransformFX.cs | effects/transform.rs | ported |
| VoronoiPrismFX.cs | effects/voronoi_prism.rs | ported |
| WireframeDepthFX.cs | effects/wireframe_depth.rs | ported |

## Compositor (manifold-renderer)

| Unity File | Rust File | Status |
|---|---|---|
| CompositorStack.cs | compositor.rs + layer_compositor.rs | ported |
| BlendMaterialCache.cs | layer_compositor.rs (integrated) | ported |
| CompositorBlend.shader | compositor_blend.wgsl | ported |
| GroupWetDryLerp.shader | wet_dry_lerp.wgsl | ported |
| ACESTonemap.shader | aces_tonemap.wgsl | ported |

## UI (manifold-ui + manifold-app)

| Unity File | Rust File | Status |
|---|---|---|
| UIState.cs | ui_state.rs | ported |
| CoordinateMapper.cs | coordinate_mapper.rs | ported |
| ClipHitTester.cs | clip_hit_tester.rs | ported |
| InteractionOverlay.cs | interaction_overlay.rs | ported |
| InputHandler.cs | input_handler.rs (app) | ported |
| EditingService.cs integration | editing_host.rs (app) | ported |
| TransportController.cs | transport_state.rs (app) | ported |
| UITree.cs | tree.rs | ported |
| UINode.cs | node.rs | ported |
| UIInputSystem.cs | input.rs | ported |
| BitmapSlider.cs | slider.rs | ported |
| BitmapText.cs | text.rs | ported |
| BitmapScrollContainer.cs | scroll_container.rs | ported |
| HeaderPanel | panels/header.rs | ported |
| TransportPanel | panels/transport.rs | ported |
| LayerHeaderPanel | panels/layer_header.rs | ported |
| FooterPanel | panels/footer.rs | ported |
| InspectorPanel | panels/inspector.rs | partial |
| ViewportPanel | panels/viewport.rs | ported |
| EffectCardPresenter | panels/effect_card.rs | ported |
| GenParamPresenter | panels/gen_param.rs | ported |
| DropdownPanel | panels/dropdown.rs | ported |
| PerfHUDPanel | panels/perf_hud.rs | ported |
| ProjectIOService.cs | — | missing |
| FileDialogService.cs | — | missing |
| ExportSection.cs | — | missing |
| TempoLaneEditor.cs | — | missing |
| ThumbnailCache.cs | — | missing |

## IO (manifold-io)

| Unity File | Rust File | Status |
|---|---|---|
| ProjectSerializer.cs | loader.rs + saver.rs | ported |
| ProjectJsonMigrator.cs | migrate.rs | ported |
| ProjectArchive.cs | saver.rs | partial |
| VideoExporter.cs | — | missing |
| ResolveFcpxmlExporter.cs | — | missing |

## Summary

| Subsystem | Ported | Partial | Missing |
|---|---|---|---|
| Data Models | 33 | 1 | 0 |
| Editing | 7 | 0 | 1 |
| Playback | 14 | 1 | 3 |
| Sync | 10 | 4 | 0 |
| Generators | 29 | 0 | 0 |
| Effects | 17 | 0 | 15 |
| Compositor | 5 | 0 | 0 |
| UI | 22 | 1 | 5 |
| IO | 3 | 1 | 2 |
| **Total** | **140** | **8** | **26** |
