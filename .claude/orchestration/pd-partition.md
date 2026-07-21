# P-D partition — PanelAction domain buckets (D-D0)

Design authority: `docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md` D5. Produced by the
dispatcher in-context (D-D0), not a Sonnet lane: the derivation is a computable
set-algebra audit over grep output, so a script is the reliable oracle and it
doubles as the D-D0 dispatcher-check (see validation footer).

**Baseline** = the sorted variant list of `pub enum PanelAction`
(`crates/manifold-ui/src/panels/mod.rs`), captured first. That list IS the
baseline (D-30, no hard number); its length is **303** (the queue's
original 303 was correct — the dispatcher's earlier ~309 prep number was a naive
regex artifact; the brace-tracking parse gives 303).

**Derivation rule (D5):** a variant's domain = the handler file whose `match
action` owns its ARM. Top-level `ui_bridge/mod.rs::dispatch` is a two-tier router:
it delegates to transport/editing/layer/marker/project and to the inspector CHAIN
(`inspector::dispatch_inspector`), which fans out to the six `ui_bridge/dispatch/*`
handlers; variants handled by an INLINE terminal arm in `mod.rs` (or with no arm
anywhere) are **Root**. Re-parenting follows the ARM, never the semantic name
(e.g. the `EffectMapping*` variants are Root because their only arm is `mod.rs`'s
app_render-intercept `=> DispatchResult::handled()`, not `dispatch/mapping.rs`).

Target enums for D-D1 (one per domain + `RootAction`; `PanelAction` becomes the sum).

## Transport -> `TransportAction`  (26)
arm owner: `ui_bridge/transport.rs::dispatch_transport`

- AutomationBackToArrangement
- BpmFieldClicked
- ClearBpm
- CycleClockAuthority
- CycleQuantize
- FpsFieldClicked
- InspectorScrolled
- InspectorSectionClicked
- OverviewScrub
- PlayPause
- Record
- ResetBpm
- ResolutionClicked
- Seek
- SelectClkDevice
- SetInsertCursor
- SetMidiClockDevice
- Stop
- TimelineScrollbarH
- ToggleAutomationArm
- ToggleAutomationMode
- ToggleLink
- ToggleMidiClock
- ToggleSyncOutput
- ZoomIn
- ZoomOut

## Editing -> `EditingAction`  (24)
arm owner: `ui_bridge/editing.rs::dispatch_editing`

- AutomationLaneRightClicked
- ClipClicked
- ClipDoubleClicked
- ClipRightClicked
- ContextAddAudioLayer
- ContextAddGeneratorLayer
- ContextAddVideoLayer
- ContextDeleteClip
- ContextDeleteLayer
- ContextDuplicateClip
- ContextDuplicateLayer
- ContextGroupSelectedLayers
- ContextImportMidi
- ContextPasteAtLayer
- ContextPasteAtTrack
- ContextSetLayerColor
- ContextSplitAtPlayhead
- ContextUngroup
- DropdownSelected
- LayerHeaderRightClicked
- TrackClicked
- TrackDoubleClicked
- TrackRightClicked
- ViewportHoverChanged

## Layer -> `LayerAction`  (23)
arm owner: `ui_bridge/layer.rs::dispatch_layer`

- AddGenClipClicked
- AddLayerClicked
- BlendModeClicked
- ChevronClicked
- CollapseLayer
- DeleteLayerClicked
- ExpandLayer
- FolderClicked
- LayerClicked
- LayerDoubleClicked
- LayerDragEnded
- LayerDragMoved
- LayerDragStarted
- MidiChannelClicked
- MidiDeviceClicked
- MidiInputClicked
- NewClipClicked
- SetBlendMode
- SetLayerAudioSend
- ToggleAnalysisOnly
- ToggleLed
- ToggleMute
- ToggleSolo

## Marker -> `MarkerAction`  (7)
arm owner: `ui_bridge/marker.rs::dispatch_marker`

- DeleteSelectedMarkers
- MarkerClicked
- MarkerDoubleClicked
- MarkerDragEnded
- MarkerDragMoved
- MarkerDragStarted
- MarkerRightClicked

## Project -> `ProjectAction`  (40)
arm owner: `ui_bridge/project.rs::dispatch_project`

- ContextClearAutomationLane
- ContextRemoveAutomationLane
- EnterPerformMode
- ExportFrame
- ExportVideo
- ExportXml
- MidiTriggerModeClicked
- NewProject
- OpenProject
- OpenRecent
- SaveProject
- SaveProjectAs
- SceneSetupAddEnvironment
- SceneSetupAddFog
- SceneSetupAddLight
- SceneSetupAddModifier
- SceneSetupAddObject
- SceneSetupDuplicateObject
- SceneSetupExposeParam
- SceneSetupImportModelClicked
- SceneSetupMoveModifier
- SceneSetupNewScene
- SceneSetupParamChanged
- SceneSetupRemoveLight
- SceneSetupRemoveModifier
- SceneSetupRemoveObject
- SelectAudioInputDevice
- SetAudioInputDevice
- SetDisplayResolution
- SetGenType
- SetMidiChannel
- SetMidiDevice
- SetMidiNote
- SetMidiTriggerMode
- SetRenderScale
- SetResolution
- SetTonemapCurve
- ToggleHdr
- ToggleLiveRecording
- ToggleMonitor

## Browser -> `BrowserAction`  (5)
arm owner: `ui_bridge/dispatch/browser.rs::dispatch_browser`

- BrowserCellRightClicked
- BrowserDeletePresetClicked
- BrowserDuplicatePresetClicked
- BrowserRenamePresetClicked
- BrowserRevealPresetClicked

## Clip -> `ClipAction`  (14)
arm owner: `ui_bridge/dispatch/clip.rs::dispatch_clip`

- ClipBpmClicked
- ClipChromeCollapseToggle
- ClipClearTriggersClicked
- ClipDetectClicked
- ClipDetectInstrumentToggled
- ClipDetectLayerClicked
- ClipDetectOnsetChanged
- ClipDetectQuantizeClicked
- ClipDetectSensitivityChanged
- ClipDetectSetLayer
- ClipDetectSetQuantize
- ClipLoopToggle
- ClipReplaceAudioClicked
- ClipWarpToggled

## Params -> `ParamsAction`  (65)
arm owner: `ui_bridge/dispatch/params.rs::dispatch_params`

- AddEffect
- AddEffectClicked
- AudioGainChanged
- AudioGainCommit
- AudioGainSnapshot
- BrowserSearchClicked
- CardRightClicked
- CopyGenerator
- EffectCardClicked
- EffectCollapseToggle
- EffectReorder
- EffectReorderGroup
- EffectToggle
- ExportPreset
- GenCardClicked
- GenCollapseToggle
- GenStringParamClicked
- GenStringParamDropdownClicked
- GenStringParamSelected
- GenTypeClicked
- ImportPreset
- LayerChromeCollapseToggle
- LayerOpacityChanged
- LayerOpacityCommit
- LayerOpacitySnapshot
- LedBrightnessChanged
- LedBrightnessCommit
- LedBrightnessSnapshot
- LedEnabledToggle
- MacroChanged
- MacroCommit
- MacroLabelRename
- MacroReset
- MacroSnapshot
- MacrosCollapseToggle
- MakePresetUnique
- MasterCollapseToggle
- MasterExitPathClicked
- MasterOpacityChanged
- MasterOpacityCommit
- MasterOpacitySnapshot
- ModConfigTabChanged
- ModsCompactToggled
- ParamChanged
- ParamCommit
- ParamEnumSet
- ParamFire
- ParamLabelRightClick
- ParamSnapshot
- ParamToggle
- PasteEffects
- PasteGenerator
- PushToLibrary
- RelightHeightFromChanged
- RelightParamChanged
- RelightParamCommit
- RelightParamSnapshot
- RelightToggle
- RemoveEffect
- RevertToLibrary
- SaveToLibrary
- SaveToProject
- SectionFoldToggled
- SetAllCardsCollapsed
- SetLedExitIndex

## Modulation -> `ModulationAction`  (26)
arm owner: `ui_bridge/dispatch/modulation.rs::dispatch_modulation`

- AudioModRemove
- AudioModSetActionKind
- AudioModSetInvert
- AudioModSetRateOfChange
- AudioModSetSource
- AudioModSetTriggerMode
- AudioModSetWrap
- AudioModShapeCommit
- AudioModShapeParamChanged
- AudioModShapeSnapshot
- AudioModStepAmountChanged
- AudioModStepAmountCommit
- AudioModStepAmountSnapshot
- AudioModToggle
- DriverConfig
- DriverToggle
- EnvDecayChanged
- EnvDecayCommit
- EnvDecaySnapshot
- EnvelopeToggle
- TargetChanged
- TargetCommit
- TargetSnapshot
- TrimChanged
- TrimCommit
- TrimSnapshot

## Mapping -> `MappingAction`  (14)
arm owner: `ui_bridge/dispatch/mapping.rs::dispatch_mapping`

- AbletonInvertToggle
- AbletonMacroInvertToggle
- AbletonMacroTrimChanged
- AbletonMacroTrimCommit
- AbletonMacroTrimSnapshot
- ClearMacroMappings
- MacroLabelRightClick
- MapMacroToAbleton
- MapParamToAbleton
- MapParamToMacro
- OpenAbletonPickerForMacro
- UnmapMacro
- UnmapMacroAbleton
- UnmapParamAbleton

## AudioSetup -> `AudioSetupAction`  (24)
arm owner: `ui_bridge/dispatch/audio_setup.rs::dispatch_audio_setup`

- AudioAddSend
- AudioCrossoverChanged
- AudioCrossoverCommit
- AudioCrossoverDragBegin
- AudioRemoveSend
- AudioRenameSend
- AudioSendFloorStep
- AudioSendGainDragBegin
- AudioSendGainDragChanged
- AudioSendGainDragCommit
- AudioSendGainSetTyped
- AudioSendGainStep
- AudioSetDevice
- AudioSetSendChannels
- AudioTriggerAdd
- AudioTriggerEnabledToggle
- AudioTriggerRemove
- AudioTriggerRowExpandToggle
- AudioTriggerSectionToggle
- AudioTriggerSetLength
- AudioTriggerSetSource
- AudioTriggerShapeCommit
- AudioTriggerShapeParamChanged
- AudioTriggerShapeSnapshot

## Root -> `RootAction`  (35)
arm owner: `ui_bridge/mod.rs::dispatch` inline terminal arms (SliderReset recurses;
SelectInspectorTab/OpenAudioSetup/OpenSceneSetup handled inline; the rest are
`=> DispatchResult::handled()/structural()` exhaustiveness stubs for actions
intercepted earlier in app_render / ui_root). All 35 verified present as real
`mod.rs` arms (none is arm-nowhere).

- AudioSendChannelClicked
- AudioSendClicked
- AudioSendGainBeginTextInput
- AudioSendLabelClicked
- AudioSetupDeviceClicked
- BeginDriverPeriodTextInput
- BeginParamTextInput
- CopyOscAddress
- EffectMappingAffineChanged
- EffectMappingAffineCommit
- EffectMappingAffineSnapshot
- EffectMappingCurve
- EffectMappingGotoNode
- EffectMappingInvert
- EffectMappingLabel
- EffectMappingRangeChanged
- EffectMappingRangeCommit
- EffectMappingRangeSnapshot
- EffectMappingSection
- OpenAbletonPickerForParam
- OpenAudioSetup
- OpenCardMapping
- OpenGeneratorGraphEditor
- OpenGraphEditor
- OpenSceneSetup
- ParamEnumDropdown
- SceneSetupAddModifierClicked
- SceneSetupBeginNumericTextInput
- SceneSetupEnumClicked
- SceneSetupOpenGraphEditor
- SceneSetupRenameLightClicked
- SceneSetupRenameObjectClicked
- SceneSetupSelectionChanged
- SelectInspectorTab
- SliderReset

## Validation (D-D0 dispatcher-check -- PASS)
- baseline variants: **303**
- sum(domains) + Root = 268 + 35 = **303** == baseline
- disjointness: **0** variants owned by 2+ handler files
- no handler match truncated by a preceding `#[cfg(test)]` (all test mods trail their dispatch fn)
- every Root variant confirmed as a real `mod.rs` inline arm (arm-nowhere = 0)
- method: `scratchpad/pd_derive.py` -- `#[cfg(test)]` blocks stripped, `//` comments stripped, references restricted to baseline variants.

## D-D1 location (executor's call, recorded)
The 12 per-domain intent enums + 12 `From<DomainAction> for PanelAction` impls live in
`crates/manifold-ui/src/panels/actions.rs` (new file; `pub mod actions;` + re-exports at
`panels::` AND `manifold_ui::` top level). `PanelAction` (in `panels/mod.rs`) is the thin
12-arm sum. Root arms: handled INLINE as a nested `match a { RootAction::… }` under the
router's `PanelAction::Root(a)` arm in `ui_bridge/mod.rs::dispatch` (no separate
`dispatch_root` — SliderReset's recursion + `inspector_select_tab` + `ctx.ui.toggle_*`
keep it in the router's scope). Inspector chain (`dispatch_inspector`) and its
`dispatch_chain_completeness` test RETIRED in the same change per the flat-12 ruling.

## D-D3 (scrub-trio annotation) -- TODO, pre-derived while warm
Snapshot/Changed/Commit trio membership per variant is P-I's kill list; annotate here in D-D3.
