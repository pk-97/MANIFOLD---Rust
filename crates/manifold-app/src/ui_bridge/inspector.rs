//! Inspector-related dispatch: effect params, drivers, envelopes, generator params,
//! master/layer/clip chrome, slider interactions.

use manifold_core::effects::{PresetInstance, ParamEnvelope, ParameterDriver};
use manifold_core::project::Project;
use manifold_editing::command::Command;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::LayerId;
use manifold_editing::commands::ableton::ChangeAbletonTrimCommand;
use manifold_core::audio_clip_detection::DetectionConfig;
use manifold_editing::commands::clip::{ChangeClipLoopCommand, ChangeClipRecordedBpmCommand};
use manifold_editing::commands::clip_detection::SetClipDetectionConfigCommand;
use manifold_editing::commands::drivers::{
    AddDriverCommand, ChangeDriverBeatDivCommand, ChangeDriverWaveformCommand, ChangeTrimCommand,
    SetDriverFreePeriodCommand, ToggleDriverEnabledCommand, ToggleDriverReversedCommand,
};
use manifold_editing::commands::audio_mod::{
    AddAudioModCommand, RemoveAudioModCommand, SetAudioModActionCommand, SetAudioModShapeCommand,
    SetAudioModSourceCommand, SetAudioModTriggerModeCommand, ToggleAudioModEnabledCommand,
};
use manifold_editing::commands::audio_setup::{
    AddAudioSendCommand, RemoveAudioSendCommand, RenameAudioSendCommand, SetAudioCrossoversCommand,
    SetAudioInputDeviceCommand, SetAudioSendChannelsCommand, SetAudioSendFloorCommand,
    SetAudioSendGainCommand,
};
use manifold_editing::commands::effect_target::{DriverTarget, EffectTarget};
use manifold_editing::commands::effects::{
    ChangeGraphParamCommand, RemoveEffectCommand, ReorderEffectCommand, ReorderEffectGroupCommand,
    SetRelightHeightFromCommand, SetRelightParamCommand, ToggleEffectCommand, ToggleRelightCommand,
};
use manifold_editing::commands::envelopes::{
    ChangeEnvelopeDecayCommand, ChangeEnvelopeTargetCommand,
};
use manifold_editing::commands::graph::SetGraphNodeParamCommand;
use manifold_editing::commands::layer::{
    AddLayerClipTriggerCommand, RemoveLayerClipTriggerCommand, SetLayerClipTriggerCommand,
};
use manifold_editing::commands::settings::{
    ChangeLayerOpacityCommand, ChangeLedBrightnessCommand, ChangeMacroCommand,
    ChangeMasterOpacityCommand, PasteGeneratorCommand,
};
use manifold_ui::{
    AudioShapeParam, DriverConfigAction, GraphParamTarget, InspectorTab, PanelAction, TrimKind,
};

use super::DispatchResult;
use super::{resolve_effects_mut, resolve_effects_read};
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

/// Send gain trim range (dB) — shared by the stepper (`AudioSendGainStep`) and
/// the D7 drag (`AudioSendGainDragChanged`/`Commit`).
const AUDIO_SEND_GAIN_MIN_DB: f32 = -24.0;
const AUDIO_SEND_GAIN_MAX_DB: f32 = 24.0;

/// Apply `edit` to the envelope matched by `param_id` on `target`, in both the
/// local UI project and the content thread (the next snapshot must not stomp
/// the live tweak). Edits the existing envelope only — no create. The unified
/// non-undoable live-drag envelope helper, for effects and generators alike:
/// the kind fork lives entirely inside `with_preset_graph_mut`.
fn graph_env_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut ParamEnvelope) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(env) = inst
            .envelopes
            .as_mut()
            .and_then(|es| es.iter_mut().find(|e| e.param_id == param_id))
        {
            edit(env);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(env) = inst
                    .envelopes
                    .as_mut()
                    .and_then(|es| es.iter_mut().find(|e| e.param_id == pid))
                {
                    edit2(env);
                }
            });
        })),
    );
}

/// Driver twin of [`graph_env_dual_edit`]: apply `edit` to the driver matched
/// by `param_id` on `target`, locally and on the content thread.
fn graph_driver_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut ParameterDriver) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(driver) = inst
            .drivers
            .as_mut()
            .and_then(|ds| ds.iter_mut().find(|d| d.param_id == param_id))
        {
            edit(driver);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(driver) = inst
                    .drivers
                    .as_mut()
                    .and_then(|ds| ds.iter_mut().find(|d| d.param_id == pid))
                {
                    edit2(driver);
                }
            });
        })),
    );
}

/// Live dual-edit of one audio modulation, mirroring [`graph_driver_dual_edit`]
/// for the green trim-handle drag: applies `edit` to the matching
/// `ParameterAudioMod` on both the UI-thread project mirror and (via
/// `MutateProjectLive`) the content thread, so the handle tracks under the
/// cursor without an undo entry per frame (the undo command lands on commit).
fn graph_audio_mod_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut manifold_core::audio_mod::ParameterAudioMod) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(m) = inst
            .audio_mods
            .as_mut()
            .and_then(|ms| ms.iter_mut().find(|a| a.param_id == param_id))
        {
            edit(m);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(m) = inst
                    .audio_mods
                    .as_mut()
                    .and_then(|ms| ms.iter_mut().find(|a| a.param_id == pid))
                {
                    edit2(m);
                }
            });
        })),
    );
}

/// Live (non-undo) edit of one `LayerClipTrigger`'s shape, mirroring
/// `graph_audio_mod_dual_edit` — applies `edit` to the UI-side `project`
/// snapshot immediately AND queues the same edit onto the content thread's
/// live project via `MutateProjectLive`, so a drag reads back correctly on
/// both sides without an undo entry per frame (the commit, on drag-end,
/// records the one undo step).
fn clip_trigger_shape_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    layer_id: &LayerId,
    index: usize,
    edit: F,
) where
    F: Fn(&mut manifold_core::audio_mod::AudioModShape) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id)
        && let Some(t) = layer.clip_triggers.get_mut(index)
    {
        edit(&mut t.shape);
    }
    let edit2 = edit.clone();
    let lid = layer_id.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&lid)
                && let Some(t) = layer.clip_triggers.get_mut(index)
            {
                edit2(&mut t.shape);
            }
        })),
    );
}

/// Resolve a `GraphParamTarget` (the card's effect-row index or generator
/// marker) to a stable `GraphTarget`, for routing through
/// `Project::with_preset_graph_mut` and the GraphTarget-keyed editing
/// commands. The single resolver behind every collapsed param/modulation
/// dispatch arm: effects address by stable `EffectId` (editor-aware via
/// `resolve_effect_id`), generators by the active layer's `LayerId`.
fn resolve_graph_target(
    gpt: &GraphParamTarget,
    editor_target: Option<&manifold_core::GraphTarget>,
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
    project: &Project,
) -> Option<manifold_core::GraphTarget> {
    match gpt {
        GraphParamTarget::Effect(idx) => {
            super::resolve_effect_id(editor_target, tab, active_layer, selection, project, *idx)
                .map(manifold_core::GraphTarget::Effect)
        }
        GraphParamTarget::Generator => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project)?;
            let lid = project.timeline.layers.get(layer_idx)?.layer_id.clone();
            Some(manifold_core::GraphTarget::Generator(lid))
        }
    }
}

/// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a (D2): resolve a card-shaped
/// `PanelAction`'s `(GraphParamTarget, ParamId)` to a converted scene row's
/// write address, `GraphTarget`, and catalog default — `None` unless
/// `param_id` resolves through `ScenePanel::resolve_scene_param`'s id map
/// (a real card/exposed param falls through to the caller's existing path).
/// The `GraphTarget` comes from the panel's own docked layer
/// (`live_layer_id`), never the active-layer context: scene rows always
/// address the panel's own generator, and active-layer resolution silently
/// no-op'd every write when the two differed. The context args stay in the
/// signature so the call sites read uniformly with `resolve_graph_target`.
#[allow(clippy::too_many_arguments)]
fn resolve_scene_write(
    ui: &UIRoot,
    project: &Project,
    _gpt: &GraphParamTarget,
    param_id: &manifold_core::effects::ParamId,
    _editor_target: Option<&manifold_core::GraphTarget>,
    _tab: InspectorTab,
    _active_layer: &Option<LayerId>,
    _selection: &SelectionState,
) -> Option<(
    manifold_ui::panels::scene_setup_panel::RowAddr,
    manifold_core::GraphTarget,
    manifold_core::effect_graph_def::EffectGraphDef,
)> {
    let (addr, _snapshot_val) = ui.scene_setup_panel.resolve_scene_param(param_id)?;
    // The scene panel edits the scene of its OWN docked layer — resolve the
    // target from the panel's `live_layer_id`, not the app's active layer.
    // Routing through `resolve_graph_target` (active layer) silently dropped
    // every converted-row write whenever the panel's layer wasn't active —
    // which is always the case in the headless harness, where no layer is
    // ever activated (the enum/drag writes all no-op'd, only the bespoke
    // layer-id-carrying scene actions worked).
    let lid = ui.scene_setup_panel.live_layer_id()?.clone();
    let target = manifold_core::GraphTarget::Generator(lid);
    let manifold_core::GraphTarget::Generator(lid) = &target else {
        return None;
    };
    let catalog_default = super::generator_catalog_default(project, lid)?;
    Some((addr, target, catalog_default))
}

/// BUG-249's sibling for plain VALUE writes (root fix, 2026-07-18): a scene
/// row whose inner (node, param) is covered by a card/user binding must edit
/// the BINDING's instance slot, never the def. A def write on a bound param
/// is structurally dead — the chain rebuild re-seeds the binding's value
/// over it, and the per-frame apply loop wins the tug-of-war whenever the
/// outer slot moves — which is exactly why the glb importer's camera
/// (`cam_orbit`/`cam_tilt`/… card bindings onto `node.orbit_camera`) never
/// responded to the Scene Setup panel. Routing the write through the slot
/// makes the panel and the perform card two views of ONE value, the same
/// resolution `resolve_mod_target` already applies to modulation.
fn scene_bound_slot(
    project: &mut Project,
    target: &manifold_core::GraphTarget,
    addr: &manifold_ui::panels::scene_setup_panel::RowAddr,
    catalog_default: &manifold_core::effect_graph_def::EffectGraphDef,
) -> Option<manifold_core::effects::ParamId> {
    project
        .with_preset_graph_mut(target, |inst| {
            inst.binding_id_for_node_param(addr.node_doc_id, &addr.param_id)
        })
        .flatten()
        // Tracking instance (`graph: None` — every freshly imported layer):
        // resolve against the catalog def it tracks instead.
        .or_else(|| {
            manifold_core::effects::binding_id_for_node_param_in(
                catalog_default,
                addr.node_doc_id,
                &addr.param_id,
            )
        })
        .map(manifold_core::effects::ParamId::from)
}

/// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a/C-P1b: descend into
/// `nodes` at `scope` (a path of group-node ids from the document root),
/// same recursive walk as `manifold-editing::commands::graph`'s private
/// `descend_level` — duplicated here because that helper isn't `pub` and
/// this crate has no other way to resolve a `RowAddr`'s `scope_path` into
/// the group body it actually lives in. `scope.split_first()`'s `None` case
/// (root) returns `nodes` unchanged; C-P1b is the first caller to actually
/// exercise a non-empty scope — Object rows living inside their own
/// `AddSceneObjectCommand` group (Color/Metallic/Roughness, D12).
fn descend_to_scope<'a>(
    nodes: &'a [manifold_core::effect_graph_def::EffectGraphNode],
    scope: &[u32],
) -> Option<&'a [manifold_core::effect_graph_def::EffectGraphNode]> {
    match scope.split_first() {
        None => Some(nodes),
        Some((gid, rest)) => {
            let group = nodes.iter().find(|n| n.id == *gid)?;
            descend_to_scope(group.group.as_ref()?.nodes.as_slice(), rest)
        }
    }
}

/// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a: read a converted scene
/// row's CURRENT value straight off the resolved `EffectGraphDef` node —
/// the read half of `SetGraphNodeParamCommand`'s write, used by `ParamCommit`
/// to compute the post-drag value to diff against the pre-drag snapshot.
/// C-P1b: walks `addr.scope_path` via `descend_to_scope` — C-P1a's
/// root-only version silently no-op'd `ParamCommit` for any scoped row,
/// which C-P1b's Color/Metallic/Roughness rows (living inside the object's
/// own group, D12) actually need.
fn read_scene_node_param(
    project: &mut Project,
    target: &manifold_core::GraphTarget,
    addr: &manifold_ui::panels::scene_setup_panel::RowAddr,
) -> Option<f32> {
    project
        .with_preset_graph_mut(target, |host| {
            let def = host.graph_def_mut().as_ref()?;
            let nodes = descend_to_scope(&def.nodes, &addr.scope_path)?;
            let node = nodes.iter().find(|n| n.id == addr.node_doc_id)?;
            match node.params.get(&addr.param_id) {
                Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) => Some(*value),
                // Enum/Int/Bool params (light `shadow_softness` is `Enum`)
                // still write as Float through `SetGraphNodeParamCommand` —
                // the primitives read both shapes (`ParamValue::Enum` or
                // `Float`, see the light primitive's `softness_idx` match) —
                // so the read half coerces them to f32 too. Without this the
                // old==new gate saw `None` and every enum row write no-op'd.
                Some(manifold_core::effect_graph_def::SerializedParamValue::Enum { value }) => Some(*value as f32),
                Some(manifold_core::effect_graph_def::SerializedParamValue::Int { value }) => Some(*value as f32),
                Some(manifold_core::effect_graph_def::SerializedParamValue::Bool { value }) => Some(*value as u8 as f32),
                _ => None,
            }
        })
        .flatten()
}

/// BUG-249 (expose-then-arm, Peter's call): resolve a modulation-family
/// action's `(target, param_id)` for BOTH row kinds through one funnel.
///
/// A converted scene row's `param_id` is the synthesized `scene.{doc}.{param}`
/// id — the modulation runtime (`modulation.rs`) only ever resolves via
/// `inst.params.get_mut(param_id)`, so storing a driver/envelope/audio-mod
/// against the synth id arms state the runtime silently drops (the whole
/// bug). The fix: a scene row's modulation always targets the REAL exposed
/// instance param bound to the inner node —
/// - already exposed → translate to the existing binding id
///   ([`PresetInstance::binding_id_for_node_param`], bundled or user-added);
/// - not exposed and `materialize` (an arm/add action) → first run the SAME
///   `ToggleNodeParamExposeCommand` the panel's mod button and the graph
///   editor's expose glyph dispatch (metadata from the primitive's own
///   `ParamDef` table), then translate. Expose + arm are two undo entries —
///   undo peels the arm first, then the exposure, which mirrors doing the
///   two clicks by hand;
/// - not exposed and `!materialize` (a config edit on a drawer that can't
///   exist yet) → `None`, and the caller swallows the action.
///
/// A non-scene `param_id` (id-map miss) resolves through
/// [`resolve_graph_target`] with the id unchanged — the pre-existing card
/// path, byte-for-byte.
#[allow(clippy::too_many_arguments)]
fn resolve_mod_target(
    ui: &UIRoot,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    gpt: &GraphParamTarget,
    param_id: &manifold_core::effects::ParamId,
    editor_target: Option<&manifold_core::GraphTarget>,
    effective_tab: InspectorTab,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
    materialize: bool,
) -> Option<(manifold_core::GraphTarget, manifold_core::effects::ParamId)> {
    let Some((addr, snapshot_val)) = ui.scene_setup_panel.resolve_scene_param(param_id) else {
        // Not a scene row — the existing exposed-param card path.
        let target =
            resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)?;
        return Some((target, param_id.clone()));
    };
    let (_, target, catalog_default) = resolve_scene_write(
        ui, project, gpt, param_id, editor_target, effective_tab, active_layer, selection,
    )?;
    let existing = project
        .with_preset_graph_mut(&target, |inst| {
            inst.binding_id_for_node_param(addr.node_doc_id, &addr.param_id)
        })
        .flatten()
        // Tracking instance (graph: None — fresh imports): the bundled
        // binding lives on the catalog def; without this fallback the arm
        // below would materialize a DUPLICATE user exposure for an
        // already-bound param.
        .or_else(|| {
            manifold_core::effects::binding_id_for_node_param_in(
                &catalog_default,
                addr.node_doc_id,
                &addr.param_id,
            )
        });
    if let Some(id) = existing {
        return Some((target, manifold_core::effects::ParamId::from(id)));
    }
    if !materialize {
        return None;
    }
    // Materialize the exposure. Same construction as `project.rs`'s
    // `SceneSetupExposeParam` arm, but with the param metadata read off the
    // primitive's own `ParamDef` table (this funnel has no panel-supplied
    // expose context — the click was a D/E/A button, not the mod button).
    let manifold_core::GraphTarget::Generator(lid) = &target else {
        return None;
    };
    let effective_def = project
        .timeline
        .find_layer_by_id(lid)
        .and_then(|(_, layer)| layer.generator_graph().cloned())
        .unwrap_or_else(|| catalog_default.clone());
    let node = super::project::find_node_by_scope(&effective_def, &addr.scope_path, addr.node_doc_id)?;
    let node_handle = node.handle.clone().unwrap_or_else(|| format!("node{}", addr.node_doc_id));
    let node_id = node.node_id.clone();
    let node_title = node.title.clone();
    let type_id = node.type_id.clone();
    let doc_params = node.params.clone();
    // Primitive-side metadata: construct the node like `snapshot.rs` does
    // (seed defaults, apply doc overrides, reconfigure) so variadic nodes
    // report their real param list. Boundary path — first arm only.
    let registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    let mut boxed = registry.construct(&type_id)?;
    let mut params: manifold_renderer::node_graph::ParamValues = ahash::AHashMap::default();
    for pd in boxed.parameters() {
        params.insert(pd.name.clone(), pd.default.clone());
    }
    for (k, v) in &doc_params {
        params.insert(std::borrow::Cow::Owned(k.clone()), v.clone().into());
    }
    boxed.reconfigure(&params);
    let pd = boxed.parameters().iter().find(|p| p.name.as_ref() == addr.param_id)?.clone();
    let (min, max) = pd.range.unwrap_or((0.0, 1.0));
    use manifold_renderer::node_graph::ParamType;
    let is_angle = matches!(pd.ty, ParamType::Angle);
    let (convert, value_labels) = if matches!(pd.ty, ParamType::Enum) {
        (
            manifold_core::effects::ParamConvert::EnumRound,
            pd.enum_values.iter().map(|s| s.to_string()).collect(),
        )
    } else {
        (manifold_core::effects::ParamConvert::Float, Vec::new())
    };
    let object_label = node_title.unwrap_or_else(|| node_handle.clone());
    let cmd = manifold_editing::commands::graph::ToggleNodeParamExposeCommand::new(
        target.clone(),
        node_id,
        addr.node_doc_id,
        node_handle,
        addr.param_id.clone(),
        true,
        catalog_default,
        format!("{object_label} \u{b7} {}", pd.label),
        min,
        max,
        snapshot_val,
        convert,
        is_angle,
        value_labels,
    )
    .with_scope(addr.scope_path.clone());
    let mut boxed_cmd: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
    boxed_cmd.execute(project);
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Execute(boxed_cmd),
    );
    let minted = project
        .with_preset_graph_mut(&target, |inst| {
            inst.binding_id_for_node_param(addr.node_doc_id, &addr.param_id)
        })
        .flatten()?;
    Some((target, manifold_core::effects::ParamId::from(minted)))
}

/// The `AbletonMappingTarget` for a resolved param `target` on `tab`, so the
/// Ableton trim/invert arms route through the shared
/// `Project::ableton_param_mappings_mut` locate-fork (effects addressed by
/// `effect_type` within master/layer — first match — generators by layer).
/// `None` for the clip tab (no clip-scoped Ableton mappings).
fn ableton_mapping_target(
    target: &manifold_core::GraphTarget,
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    project: &Project,
    param_id: &manifold_core::effects::ParamId,
) -> Option<manifold_core::ableton_mapping::AbletonMappingTarget> {
    use manifold_core::ableton_mapping::AbletonMappingTarget as T;
    match target {
        manifold_core::GraphTarget::Effect(eid) => {
            let effect_type = project.find_effect_by_id(eid)?.effect_type().clone();
            match tab {
                InspectorTab::Master => Some(T::MasterEffect {
                    effect_type,
                    param_id: param_id.clone(),
                }),
                InspectorTab::Layer | InspectorTab::Group => Some(T::LayerEffect {
                    layer_id: active_layer.clone()?,
                    effect_type,
                    param_id: param_id.clone(),
                }),
                InspectorTab::Clip => None,
            }
        }
        manifold_core::GraphTarget::Generator(lid) => Some(T::GenParam {
            layer_id: lid.clone(),
            param_id: param_id.clone(),
        }),
    }
}

/// The `MacroMappingTarget` for a resolved param `target` (macro twin of
/// [`ableton_mapping_target`]). Effects address by stable `EffectId`, which
/// reaches master / layer / clip effects directly — so the macro drives the
/// exact instance the user mapped, even with two same-type effects on a layer.
fn macro_mapping_target(
    target: &manifold_core::GraphTarget,
    param_id: &manifold_core::effects::ParamId,
) -> Option<manifold_core::MacroMappingTarget> {
    use manifold_core::MacroMappingTarget as T;
    match target {
        manifold_core::GraphTarget::Effect(eid) => Some(T::Effect {
            effect_id: eid.clone(),
            param_id: param_id.clone(),
        }),
        manifold_core::GraphTarget::Generator(lid) => Some(T::GenParam {
            layer_id: lid.clone(),
            param_id: param_id.clone(),
        }),
    }
}

/// The `(min, max)` range for `param_id` on `inst`, graph-authority-first:
/// the per-instance graph `preset_metadata` (where graph-backed presets —
/// notably generators — carry their authoritative ranges) takes precedence
/// over the registry, falling back to [`PresetInstance::resolve_param`] (and
/// `(0.0, 1.0)` if unresolved). Unifies the old per-kind range lookups.
fn resolve_param_range(inst: &PresetInstance, param_id: &str) -> (f32, f32) {
    // The manifest entry is the single range authority: calibration edits
    // `spec.min`/`spec.max` in place (D6), so the old override-then-catalog
    // lookup collapses to reading the `Param.spec`.
    inst.params
        .get(param_id)
        .map(|p| (p.spec.min, p.spec.max))
        .unwrap_or((0.0, 1.0))
}

/// The preset graph def to fork or export for a resolved `target`: the
/// per-instance diverged graph if the instance carries one, else the catalog
/// canonical def from the loaded preset view. Paired with the current preset
/// id (the export filename stem). One path for both kinds — the fork / export
/// dispatch arms resolve a `GraphTarget` (effect or generator) and hand it
/// here, so make-unique / export behave identically on either card.
fn preset_source_def(
    target: &manifold_core::GraphTarget,
    project: &Project,
) -> Option<(
    manifold_core::effect_graph_def::EffectGraphDef,
    manifold_core::PresetTypeId,
)> {
    let inst = project.preset_instance(target)?;
    let preset_id = inst.effect_type().clone();
    let mut def = inst.graph.clone().or_else(|| {
        manifold_renderer::node_graph::loaded_preset_view_by_id(&preset_id)
            .map(|v| (*v.canonical_def).clone())
    })?;
    // Snapshot the card's current slider values into the def's defaults so Make
    // Unique / Export freeze the configured look rather than the stock template.
    // The live values stay on the instance (the performance surface); this only
    // makes the def reproduce them on a later add/import/load.
    inst.snapshot_values_into_def(&mut def);
    Some((def, preset_id))
}

/// Apply a project-level audio-setup command locally and forward it to the
/// content thread. The shared tail for every Audio Setup action (they all
/// mutate `project.audio_setup` and need no target resolution).
fn audio_setup_command(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    mut cmd: Box<dyn manifold_editing::command::Command + Send>,
) -> DispatchResult {
    cmd.execute(project);
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Execute(cmd),
    );
    DispatchResult::structural()
}

/// Apply an edit to a clip's `DetectionConfig` and re-place its triggers from the
/// cached analysis. Reads the current config (or default), mutates it, records the
/// config change (local + content thread), then asks the orchestrator to re-plan
/// — instant, no backend run. See `docs/AUDIO_CLIP_DETECTION_DESIGN.md`.
fn apply_detection_edit(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    clip_id: &manifold_core::ClipId,
    mutate: impl FnOnce(&mut DetectionConfig),
) {
    use crate::content_command::ContentCommand;
    let mut config = project
        .timeline
        .find_clip_by_id(clip_id)
        .and_then(|c| c.audio_detection.as_ref())
        .map(|d| d.config.clone())
        .unwrap_or_default();
    mutate(&mut config);

    let mut cmd: Box<dyn manifold_editing::command::Command + Send> =
        Box::new(SetClipDetectionConfigCommand::new(clip_id.clone(), config));
    cmd.execute(project);
    ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
    ContentCommand::send(content_tx, ContentCommand::ReplanClip(clip_id.clone()));
}

/// `manifold_ui::panels::browser_popup::BrowserPopupMode` stands in for
/// `manifold_core::preset_def::PresetKind` on the UI side of the browser's
/// management actions (PRESET_LIBRARY_DESIGN P5) — `manifold-ui` mirrors core
/// types rather than depending on `manifold-core` (see `BrowserCellContext`'s
/// doc comment). `Node` never reaches these arms in practice: the browser
/// only classifies a source (and therefore only ever fires
/// `BrowserCellRightClicked`) for the Effect/Generator pickers, never the
/// graph-editor's node picker — degrade to `Effect` rather than panic if that
/// invariant is ever violated.
fn browser_mode_to_kind(
    mode: manifold_ui::panels::browser_popup::BrowserPopupMode,
) -> manifold_core::preset_def::PresetKind {
    use manifold_ui::panels::browser_popup::BrowserPopupMode;
    match mode {
        BrowserPopupMode::Effect | BrowserPopupMode::Node => {
            manifold_core::preset_def::PresetKind::Effect
        }
        BrowserPopupMode::Generator => manifold_core::preset_def::PresetKind::Generator,
    }
}

pub(super) fn dispatch_inspector(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
    active_layer: &mut Option<LayerId>,
    drag_snapshot: &mut Option<f32>,
    trim_snapshot: &mut Option<(f32, f32)>,
    target_snapshot: &mut Option<f32>,
    decay_snapshot: &mut Option<f32>,
    audio_shape_snapshot: &mut Option<manifold_core::audio_mod::AudioModShape>,
    audio_action_snapshot: &mut Option<manifold_core::audio_mod::TriggerAction>,
    audio_crossover_snapshot: &mut Option<(f32, f32)>,
    audio_send_gain_drag_snapshot: &mut Option<f32>,
    active_inspector_drag: &mut Option<crate::app::ActiveInspectorDrag>,
    editor_target: Option<&manifold_core::GraphTarget>,
) -> DispatchResult {
    use crate::content_command::ContentCommand;

    // The single-effect VALUE / expose / mapping arms address their instance by
    // stable `EffectId` via `super::resolve_effect_id(editor_target, …)` and
    // ignore `effective_tab` / `active_layer` when the editor supplies an
    // identity. The MODULATION arms (drivers, layer-stored envelopes, trims,
    // envelope targets) still resolve positionally through `(tab, active_layer)`
    // + the effect's row index, so they need a tab/layer that points at the
    // editor's WATCHED effect — not the main window's selection — when a card
    // action is dispatched from the editor. `editor_dispatch_context` expresses
    // the editor's identity in those positional terms (Master / its Layer /
    // Clip), and is byte-identical to the inspector's own context on the
    // perform path (`editor_target == None`).
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(
        editor_target,
        project,
        ui.inspector.last_effect_tab(),
        active_layer,
    );
    // Shadow the &mut param: no arm in this function mutates `active_layer`, so an
    // immutable shadow is sound and routes every downstream resolver through the
    // effective layer.
    let active_layer: &Option<LayerId> = &effective_active_layer;

    match action {
        // ── Macros panel collapse ─────────────────────────────────
        PanelAction::MacrosCollapseToggle => {
            ui.inspector.macros_panel_mut().toggle_collapsed();
            DispatchResult::structural()
        }

        // ── Macro sliders ─────────────────────────────────────────
        PanelAction::MacroSnapshot(idx) => {
            let idx = *idx;
            if idx < manifold_core::macro_bank::MACRO_COUNT {
                let value = project.settings.macro_bank.slots[idx].value;
                *drag_snapshot = Some(value);
                // Macros ride in every ModulationSnapshot block, so the drag
                // must be guarded or the per-tick apply stomps it (undo-race
                // regression, 2026-07-18).
                *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::Macro { idx, value });
            }
            DispatchResult::handled()
        }
        PanelAction::MacroChanged(idx, val) => {
            let idx = *idx;
            let val = *val;
            if let Some(crate::app::ActiveInspectorDrag::Macro { idx: di, value }) =
                active_inspector_drag
                && *di == idx
            {
                *value = val;
            }
            manifold_core::macro_bank::MacroBank::apply_macro(project, idx, val);
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    manifold_core::macro_bank::MacroBank::apply_macro(p, idx, val);
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::MacroCommit(idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                let idx = *idx;
                if idx < manifold_core::macro_bank::MACRO_COUNT {
                    let new_val = project.settings.macro_bank.slots[idx].value;
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeMacroCommand::new(idx, old_val, new_val);
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::MacroReset(idx) => {
            let idx = *idx;
            if idx < manifold_core::macro_bank::MACRO_COUNT {
                let old = project.settings.macro_bank.slots[idx].value;
                if old.abs() > f32::EPSILON {
                    manifold_core::macro_bank::MacroBank::apply_macro(project, idx, 0.0);
                    let cmd = ChangeMacroCommand::new(idx, old, 0.0);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::MacroLabelRename(_) => DispatchResult::handled(),

        // ── Master chrome ──────────────────────────────────────────
        PanelAction::MasterOpacitySnapshot => {
            *drag_snapshot = Some(project.settings.master_opacity);
            *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::MasterOpacity(
                project.settings.master_opacity,
            ));
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityChanged(val) => {
            project.settings.master_opacity = *val;
            if let Some(crate::app::ActiveInspectorDrag::MasterOpacity(v)) = active_inspector_drag {
                *v = *val;
            }
            let v = *val;
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    p.settings.master_opacity = v;
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                let new_val = project.settings.master_opacity;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeMasterOpacityCommand::new(old_val, new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        // ── Audio-layer gain slider (layer header) ─────────────────
        PanelAction::AudioGainSnapshot(id) => {
            *drag_snapshot = project
                .timeline
                .find_layer_by_id(id)
                .map(|(_, l)| l.audio_gain_db);
            DispatchResult::handled()
        }
        PanelAction::AudioGainChanged(id, db) => {
            let db = *db;
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(id) {
                layer.audio_gain_db = db;
                let id = id.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        if let Some((_, l)) = p.timeline.find_layer_by_id_mut(&id) {
                            l.audio_gain_db = db;
                        }
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::AudioGainCommit(id) => {
            if let Some(old_db) = drag_snapshot.take()
                && let Some((_, layer)) = project.timeline.find_layer_by_id(id)
            {
                let new_db = layer.audio_gain_db;
                if (old_db - new_db).abs() > f32::EPSILON {
                    let cmd = manifold_editing::commands::layer::SetLayerAudioGainCommand::new(
                        layer.layer_id.clone(),
                        old_db,
                        new_db,
                    );
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::MasterCollapseToggle => {
            ui.inspector.master_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::MasterExitPathClicked => {
            // Handled by try_open_dropdown in ui_root.rs — opens exit path dropdown.
            DispatchResult::handled()
        }
        PanelAction::SetLedExitIndex(idx) => {
            let idx = *idx;
            project.settings.led_exit_index = idx;
            // Push to content thread so the LED pipeline picks it up
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_exit_index = idx;
                })),
            );
            DispatchResult::handled()
        }
        // ── LED enabled toggle ───────────────────────────────────
        PanelAction::LedEnabledToggle => {
            let new_enabled = !content_state.led_enabled;
            // Persist the new ON/OFF state in project settings so the LED
            // pipeline auto-initialises on project load.
            project.settings.led_enabled = new_enabled;
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_enabled = new_enabled;
                })),
            );
            if new_enabled {
                let settings = manifold_led::LedSettings {
                    enabled: true,
                    ..Default::default()
                };
                ContentCommand::send(
                    content_tx,
                    ContentCommand::InitLedOutput(Box::new(settings)),
                );
            } else {
                ContentCommand::send(content_tx, ContentCommand::ShutdownLedOutput);
            }
            DispatchResult::handled()
        }

        // ── LED brightness ───────────────────────────────────────
        PanelAction::LedBrightnessSnapshot => {
            *drag_snapshot = Some(project.settings.led_brightness);
            *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::LedBrightness(
                project.settings.led_brightness,
            ));
            DispatchResult::handled()
        }
        PanelAction::LedBrightnessChanged(val) => {
            project.settings.led_brightness = *val;
            if let Some(crate::app::ActiveInspectorDrag::LedBrightness(v)) = active_inspector_drag {
                *v = *val;
            }
            let v = *val;
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_brightness = v;
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::LedBrightnessCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                let new_val = project.settings.led_brightness;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeLedBrightnessCommand::new(old_val, new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        // ── Layer chrome ───────────────────────────────────────────
        PanelAction::LayerOpacitySnapshot => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(idx)
            {
                *drag_snapshot = Some(layer.opacity);
                *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::LayerOpacity {
                    layer_id: layer.layer_id.clone(),
                    value: layer.opacity,
                });
            }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityChanged(val) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(idx) = layer_idx {
                if let Some(layer) = project.timeline.layers.get_mut(idx) {
                    layer.opacity = *val;
                }
                if let Some(crate::app::ActiveInspectorDrag::LayerOpacity { value, .. }) =
                    active_inspector_drag
                {
                    *value = *val;
                }
                let v = *val;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id) {
                            layer.opacity = v;
                        }
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityCommit => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(old_val) = drag_snapshot.take()
                && let Some(idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(idx)
            {
                let layer_id = layer.layer_id.clone();
                let new_val = layer.opacity;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeLayerOpacityCommand::new(layer_id, old_val, new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::LayerChromeCollapseToggle => {
            ui.inspector.layer_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        // ── Clip chrome ────────────────────────────────────────────
        PanelAction::ClipChromeCollapseToggle => {
            ui.inspector.clip_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::ClipBpmClicked => DispatchResult::handled(),
        PanelAction::ClipWarpToggled => {
            // Audio warp toggle: off (recorded_bpm 0, native speed) ⇄ on (lock to
            // the project tempo as a sensible default). One BPM command, which
            // also rescales the clip's timeline length to hold the audio span.
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                let project_bpm = project.settings.bpm.0;
                if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                    let old_bpm = clip.recorded_bpm;
                    let new_bpm = if old_bpm > 0.0 { 0.0 } else { project_bpm };
                    let cmd = ChangeClipRecordedBpmCommand::new(clip_id, old_bpm, new_bpm);
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectClicked => {
            // Per-clip detection: analyze the selected audio clip's file and place
            // its triggers. The orchestrator (content thread) does the work and the
            // result syncs back; status shows via the global percussion status.
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                ContentCommand::send(content_tx, ContentCommand::DetectClip(clip_id.clone()));
            }
            DispatchResult::handled()
        }
        PanelAction::ClipClearTriggersClicked => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::ClearClipTriggers(clip_id.clone()),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::ClipReplaceAudioClicked => {
            // Replace the clip's source file (TIMELINE_INGEST_DESIGN D6/D7): a
            // native file dialog picks the new file, `ReplaceAudioFileCommand`
            // swaps path/duration/in_point/BPM and clears the cached analysis
            // while keeping the detection config, and every clip this audio clip
            // generated (tagged `detection_source`) is deleted in the same
            // undoable step — a stale trigger for a song that no longer plays is
            // worse than none. Detection is never re-run here; it stays manual.
            use manifold_editing::command::{Command, CompositeCommand};
            use manifold_editing::commands::clip::{DeleteClipCommand, ReplaceAudioFileCommand};
            if let Some(clip_id) = selection.primary_selected_clip_id.clone()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        "Audio",
                        &["wav", "mp3", "flac", "aif", "aiff", "ogg", "m4a", "aac"],
                    )
                    .pick_file()
                && let Some(clip) = project.timeline.find_clip_by_id(&clip_id)
            {
                let new_path = path.to_string_lossy().into_owned();
                let new_source_duration = crate::project_io::audio_source_duration(&new_path);
                let replace = ReplaceAudioFileCommand::new(
                    clip_id.clone(),
                    clip.audio_file_path.clone(),
                    new_path,
                    clip.source_duration,
                    new_source_duration,
                    clip.in_point,
                    clip.recorded_bpm,
                    clip.audio_detection.clone(),
                );
                let mut commands: Vec<Box<dyn Command>> = vec![Box::new(replace)];
                for layer in project.timeline.layers.iter() {
                    let layer_id = layer.layer_id.clone();
                    for generated in layer
                        .clips
                        .iter()
                        .filter(|c| c.detection_source.as_ref() == Some(&clip_id))
                    {
                        commands.push(Box::new(DeleteClipCommand::new(
                            generated.clone(),
                            layer_id.clone(),
                        )));
                    }
                }
                // Always composite (even for just the replace) so the undo
                // stack always sees one "Replace Audio File" step regardless
                // of how many generated clips came along.
                let mut cmd: Box<dyn Command + Send> = Box::new(CompositeCommand::new(
                    commands,
                    "Replace Audio File".to_string(),
                ));
                cmd.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectInstrumentToggled(idx) => {
            let idx = *idx;
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.enabled = !inst.enabled;
                    }
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSensitivityChanged(idx, value) => {
            let (idx, value) = (*idx, *value);
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.sensitivity = value.clamp(0.0, 1.0);
                    }
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectOnsetChanged(ms) => {
            let secs = manifold_core::Seconds((*ms / 1000.0) as f64);
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| {
                    c.onset_compensation = secs;
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSetQuantize(step) => {
            let step = *step;
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| match step {
                    Some(beats) => {
                        c.quantize_on = true;
                        c.quantize_step_beats = beats;
                    }
                    None => c.quantize_on = false,
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSetLayer(idx, layer) => {
            let (idx, layer) = (*idx, layer.clone());
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.target_layer = layer;
                    }
                });
            }
            DispatchResult::structural()
        }
        // The open actions are consumed by UIRoot::try_open_dropdown before
        // dispatch; these arms are defensive no-ops.
        PanelAction::ClipDetectQuantizeClicked | PanelAction::ClipDetectLayerClicked(_) => {
            DispatchResult::handled()
        }
        PanelAction::ClipLoopToggle => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                    let old_loop = clip.is_looping;
                    let old_dur = clip.loop_duration_beats;
                    let cmd =
                        ChangeClipLoopCommand::new(clip_id, old_loop, !old_loop, old_dur, old_dur);
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }
        // BUG-061: the clip in-point ("slip") slider and its right-click reset
        // were removed (`ClipSlipSnapshot`/`Changed`/`Commit`/`RightClick`) —
        // dead code with no emitter (the slip UI itself was already gone;
        // `clip_chrome.rs`'s `set_slip_range`/`sync_slip` were empty stubs).
        // The clip LOOP-DURATION trio (`ClipLoopSnapshot`/`Changed`/`Commit`)
        // was dead for the same reason and removed alongside it.
        // `ClipLoopToggle` is a real, live toggle (is_looping) — kept above.

        // ── Effect operations ──────────────────────────────────────
        PanelAction::EffectToggle(fx_idx) => {
            let tab = effective_tab;
            let selected = ui.inspector.get_selected_effect_indices();
            // If clicked effect is part of multi-selection, apply to all selected
            let indices: Vec<usize> = if selected.len() > 1 && selected.contains(fx_idx) {
                selected
            } else {
                vec![*fx_idx]
            };
            // New state = inverse of the clicked card, applied to every selected.
            let new_enabled = super::resolve_effect_id(
                editor_target,
                tab,
                active_layer,
                selection,
                project,
                *fx_idx,
            )
            .and_then(|eid| project.find_effect_by_id(&eid).map(|fx| !fx.enabled))
            .unwrap_or(true);
            // Resolve every affected card to its stable id + current state. The
            // editor toggles its single watched effect (id wins over `idx`); the
            // inspector resolves each selected index against its own context.
            let targets: Vec<(manifold_core::EffectId, bool)> = indices
                .iter()
                .filter_map(|&idx| {
                    let eid = super::resolve_effect_id(
                        editor_target,
                        tab,
                        active_layer,
                        selection,
                        project,
                        idx,
                    )?;
                    let enabled = project.find_effect_by_id(&eid)?.enabled;
                    Some((eid, enabled))
                })
                .collect();
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for (eid, old_enabled) in &targets {
                if *old_enabled != new_enabled {
                    commands.push(Box::new(ToggleEffectCommand::new(
                        eid.clone(),
                        *old_enabled,
                        new_enabled,
                    )));
                }
            }
            // Apply locally for immediate visual feedback.
            for (eid, _) in &targets {
                if let Some(fx) = project.find_effect_by_id_mut(eid) {
                    fx.enabled = new_enabled;
                }
            }
            if !commands.is_empty() {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::ExecuteBatch(commands, "Toggle effects".into()),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::EffectCollapseToggle(fx_idx) => {
            let tab = effective_tab;
            let selected = ui.inspector.get_selected_effect_indices();
            // If clicked effect is part of multi-selection, apply to all selected
            let indices: Vec<usize> = if selected.len() > 1 && selected.contains(fx_idx) {
                selected
            } else {
                vec![*fx_idx]
            };
            let new_collapsed;
            {
                let (effects_mut, _target) =
                    resolve_effects_mut(tab, project, active_layer, selection);
                if let Some(effects) = effects_mut {
                    new_collapsed = effects.get(*fx_idx).map(|fx| !fx.collapsed).unwrap_or(true);
                    for &idx in &indices {
                        if let Some(fx) = effects.get_mut(idx) {
                            fx.collapsed = new_collapsed;
                        }
                    }
                } else {
                    new_collapsed = true;
                }
            }
            // Send to content thread so snapshot sync doesn't overwrite
            let target = super::resolve_effect_target(tab, active_layer, project);
            let indices_owned = indices;
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    let effects = match &target {
                        EffectTarget::Master => Some(&mut p.settings.master_effects),
                        EffectTarget::Layer { layer_id } => p
                            .timeline
                            .find_layer_by_id_mut(layer_id)
                            .map(|(_, l)| l.effects_mut()),
                    };
                    if let Some(effects) = effects {
                        for &idx in &indices_owned {
                            if let Some(fx) = effects.get_mut(idx) {
                                fx.collapsed = new_collapsed;
                            }
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        PanelAction::SetAllCardsCollapsed { collapsed } => {
            // Collapse/expand every effect card in the active column at once.
            // Mirrors EffectCollapseToggle's two-write pattern (snapshot now,
            // MutateProject so the content thread's snapshot doesn't overwrite).
            let tab = effective_tab;
            let collapsed = *collapsed;
            {
                let (effects_mut, _target) =
                    resolve_effects_mut(tab, project, active_layer, selection);
                if let Some(effects) = effects_mut {
                    for fx in effects.iter_mut() {
                        fx.collapsed = collapsed;
                    }
                }
            }
            let target = super::resolve_effect_target(tab, active_layer, project);
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    let effects = match &target {
                        EffectTarget::Master => Some(&mut p.settings.master_effects),
                        EffectTarget::Layer { layer_id } => p
                            .timeline
                            .find_layer_by_id_mut(layer_id)
                            .map(|(_, l)| l.effects_mut()),
                    };
                    if let Some(effects) = effects {
                        for fx in effects.iter_mut() {
                            fx.collapsed = collapsed;
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        PanelAction::ModConfigTabChanged => {
            // The card already switched its own active-tab UI state in
            // handle_click; this just forces a rebuild so the drawer repaints
            // with the newly-selected config. No model mutation.
            DispatchResult::structural()
        }
        PanelAction::SectionFoldToggled => {
            // D5 — the card already flipped its own `section_folded` UI-only
            // state in handle_click; this just forces a rebuild so the
            // folded/unfolded rows repaint. No model mutation (fold state is
            // workspace-local, never serialized).
            DispatchResult::structural()
        }
        PanelAction::ModsCompactToggled => {
            // §6b — the inspector already flipped its own compact flag in
            // route_click; rebuild so every card hides/shows its mod drawers.
            // No model mutation.
            DispatchResult::structural()
        }
        PanelAction::EffectCardClicked(_) => {
            // Deselect generator card when an effect card is clicked
            if let Some(gp) = ui.inspector.gen_params_mut() {
                gp.update_selection_visual(&mut ui.tree, false);
            }
            let tree = &mut ui.tree;
            let inspector = &mut ui.inspector;
            inspector.apply_selection_visuals(tree);
            DispatchResult::handled()
        }
        // BUG-061: the old bespoke per-param right-click reset action was
        // deleted — reset now rides the generic `SliderReset` trio
        // (`ParamSnapshot`/`ParamChanged`/`ParamCommit` below), reusing
        // these same handlers instead of a bespoke code path. Known
        // trade-off: this drops the eased "value snap-back" fill animation
        // the old handler drove via `begin_value_snapback` (D15) — the value
        // now jumps to default like any other committed change instead of
        // easing to it. `begin_value_snapback` itself is left in place in
        // manifold-ui (still exercised by its own unit tests) but has no
        // remaining production caller.
        PanelAction::ParamSnapshot(gpt, param_id) => {
            // SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a (D2): a converted
            // scene row's `param_id` is a synthesized owned id, not a real
            // exposed slot on the generator's `PresetInstance` — resolve it
            // through the scene panel's per-frame id map FIRST; a miss falls
            // through to the existing exposed-param path below unchanged.
            if let Some((_, val)) = ui.scene_setup_panel.resolve_scene_param(param_id)
                && let Some((addr, target, catalog_default)) = resolve_scene_write(
                    ui, project, gpt, param_id, editor_target, effective_tab, active_layer, selection,
                )
            {
                // Bound row (see `scene_bound_slot`): the drag edits the
                // binding's instance slot, so snapshot THAT value and hold
                // the real exposed id — the `Param` drag guard restores
                // through the manifest, which is exactly right here.
                if let Some(pid) = scene_bound_slot(project, &target, &addr, &catalog_default) {
                    let slot_val = project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.params
                                .contains(pid.as_ref())
                                .then(|| inst.get_base_param(pid.as_ref()))
                        })
                        .flatten()
                        .unwrap_or(val);
                    *drag_snapshot = Some(slot_val);
                    *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::Param {
                        target,
                        param_id: pid,
                        value: slot_val,
                    });
                    return DispatchResult::handled();
                }
                *drag_snapshot = Some(val);
                // NOT the `Param` variant: a scene row's `param_id` is
                // synthesized, so `Param`'s manifest `set_param` restore
                // would be a silent no-op — the guard must hold the row's
                // real write address (undo-race fix, 2026-07-18).
                *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::SceneParam {
                    target,
                    addr,
                    catalog_default: Box::new(catalog_default),
                    value: val,
                });
                return DispatchResult::handled();
            }
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let val = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(val) = val {
                    // Touch-to-select (P5, `docs/AUTOMATION_LANES_DESIGN.md`
                    // §7 addendum): the ONE funnel every param drag fires
                    // through, once per touch. Layer-scoped only (Master/Clip
                    // tabs have no layer for the chooser to live on, per §7's
                    // "automation lives on the layer").
                    if effective_tab.is_layer_scope()
                        && let Some(layer_id) = active_layer.clone()
                    {
                        selection.set_chosen_automation_param(
                            layer_id,
                            crate::editing_host::to_ui_graph_target(&target),
                            param_id.clone(),
                        );
                    }
                    *drag_snapshot = Some(val);
                    *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::Param {
                        target,
                        param_id: param_id.clone(),
                        value: val,
                    });
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ParamChanged(gpt, param_id, val) => {
            // C-P1a (D2/D4): motion writes for a converted scene row go
            // through the SAME `SetGraphNodeParamCommand` shape the panel's
            // old per-tick `SceneSetupParamChanged` used — but as a LIVE,
            // non-undoable write (`execute()` called and discarded locally,
            // `MutateProjectLive` on the content thread), never
            // `ContentCommand::Execute`. That's the whole cadence fix D4
            // names: the command shape was always correct, only the call
            // site pushed an undo entry per motion tick.
            if let Some((addr, target, catalog_default)) =
                resolve_scene_write(ui, project, gpt, param_id, editor_target, effective_tab, active_layer, selection)
            {
                // Bound row → live-write the instance slot, mirroring the
                // exposed-param motion path below (see `scene_bound_slot`).
                if let Some(pid) = scene_bound_slot(project, &target, &addr, &catalog_default) {
                    project.with_preset_graph_mut(&target, |inst| {
                        inst.set_base_param(pid.as_ref(), *val);
                    });
                    if let Some(crate::app::ActiveInspectorDrag::Param { value, .. }) =
                        active_inspector_drag
                    {
                        *value = *val;
                    }
                    let v = *val;
                    let t = target.clone();
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::MutateProjectLive(Box::new(move |p| {
                            p.with_preset_graph_mut(&t, |inst| {
                                inst.set_base_param(pid.as_ref(), v);
                            });
                        })),
                    );
                    return DispatchResult::handled();
                }
                let mut cmd = SetGraphNodeParamCommand::new(
                    target.clone(),
                    addr.node_doc_id,
                    addr.param_id.clone(),
                    manifold_core::effect_graph_def::SerializedParamValue::Float { value: *val },
                    catalog_default.clone(),
                )
                .with_scope(addr.scope_path.clone());
                cmd.execute(project);
                if let Some(crate::app::ActiveInspectorDrag::SceneParam { value, .. }) =
                    active_inspector_drag
                {
                    *value = *val;
                }
                let addr2 = addr.clone();
                let v = *val;
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        let mut cmd = SetGraphNodeParamCommand::new(
                            target,
                            addr2.node_doc_id,
                            addr2.param_id.clone(),
                            manifold_core::effect_graph_def::SerializedParamValue::Float { value: v },
                            catalog_default,
                        )
                        .with_scope(addr2.scope_path.clone());
                        cmd.execute(p);
                    })),
                );
                return DispatchResult::handled();
            }
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                project.with_preset_graph_mut(&target, |inst| {
                    inst.set_base_param(param_id.as_ref(), *val);
                });
                if let Some(crate::app::ActiveInspectorDrag::Param { value, .. }) =
                    active_inspector_drag
                {
                    *value = *val;
                }
                let pid = param_id.clone();
                let v = *val;
                let t = target.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        p.with_preset_graph_mut(&t, |inst| {
                            inst.set_base_param(pid.as_ref(), v);
                        });
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::ParamCommit(gpt, param_id) => {
            // C-P1a (D2/D4): release commits ONE `SetGraphNodeParamCommand`
            // through the undo-tracked `ContentCommand::Execute` path —
            // exactly what `SceneSetupParamChanged`'s dispatch arm already
            // does per-tick today; the fix is that this now fires ONCE per
            // gesture instead of once per motion event.
            if let Some(old_val) = drag_snapshot.take()
                && let Some((addr, target, catalog_default)) = resolve_scene_write(
                    ui, project, gpt, param_id, editor_target, effective_tab, active_layer, selection,
                )
            {
                // Bound row → the drag moved the instance slot; commit the
                // same `ChangeGraphParamCommand` an exposed card commit uses.
                if let Some(pid) = scene_bound_slot(project, &target, &addr, &catalog_default) {
                    let new_val = project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.params
                                .contains(pid.as_ref())
                                .then(|| inst.get_base_param(pid.as_ref()))
                        })
                        .flatten();
                    if let Some(new_val) = new_val
                        && (old_val - new_val).abs() > f32::EPSILON
                    {
                        let cmd = ChangeGraphParamCommand::new(target, pid, old_val, new_val);
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                    *active_inspector_drag = None;
                    return DispatchResult::handled();
                }
                let new_val = read_scene_node_param(project, &target, &addr);
                if let Some(new_val) = new_val
                    && (old_val - new_val).abs() > f32::EPSILON
                {
                    // `.with_previous(..)`: self-capture would read the
                    // graph's CURRENT (post-drag) value at execute time —
                    // the live `MutateProjectLive` ticks already got there
                    // first — recording a no-op `previous == new`. Seed the
                    // real pre-drag value we've held since `ParamSnapshot`
                    // instead (see the method's doc comment).
                    let cmd = SetGraphNodeParamCommand::new(
                        target,
                        addr.node_doc_id,
                        addr.param_id.clone(),
                        manifold_core::effect_graph_def::SerializedParamValue::Float { value: new_val },
                        catalog_default,
                    )
                    .with_scope(addr.scope_path.clone())
                    .with_previous(Some(manifold_core::effect_graph_def::SerializedParamValue::Float {
                        value: old_val,
                    }));
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
                *active_inspector_drag = None;
                return DispatchResult::handled();
            }
            if let Some(old_val) = drag_snapshot.take()
                && let Some(target) =
                    resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let new_val = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(new_val) = new_val
                    && (old_val - new_val).abs() > f32::EPSILON
                {
                    let cmd = ChangeGraphParamCommand::new(
                        target,
                        param_id.clone(),
                        old_val,
                        new_val,
                    );
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        // BUG-250: an enum dropdown pick — one atomic write, one undo unit,
        // no drag. Scene rows route through `resolve_scene_write` +
        // `SetGraphNodeParamCommand` (`with_previous` seeded from the
        // pre-pick value, same as `ParamCommit`); real card params take
        // `ParamToggle`'s read-old/write-new `ChangeGraphParamCommand`
        // shape. Both sides write the UI project locally for immediate
        // re-render, exactly as `ParamChanged`/`ParamToggle` already do.
        PanelAction::ParamEnumSet(gpt, param_id, new_val) => {
            if let Some((addr, target, catalog_default)) = resolve_scene_write(
                ui, project, gpt, param_id, editor_target, effective_tab, active_layer, selection,
            ) {
                // Bound row → one atomic slot write + one undo unit, the
                // exposed-param `ParamToggle` shape (see `scene_bound_slot`).
                if let Some(pid) = scene_bound_slot(project, &target, &addr, &catalog_default) {
                    let old_val = project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.params
                                .contains(pid.as_ref())
                                .then(|| inst.get_base_param(pid.as_ref()))
                        })
                        .flatten();
                    if let Some(old_val) = old_val
                        && (old_val - *new_val).abs() > f32::EPSILON
                    {
                        project.with_preset_graph_mut(&target, |inst| {
                            inst.set_base_param(pid.as_ref(), *new_val);
                        });
                        let cmd = ChangeGraphParamCommand::new(target, pid, old_val, *new_val);
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                    return DispatchResult::handled();
                }
                // `read_scene_node_param` only sees STORED params — a def
                // node omits keys still at their declared default (an
                // untouched light has no `shadow_softness` key at all), so
                // fall back to the panel's displayed snapshot from the id
                // map, which resolves declared defaults (same source
                // `ParamSnapshot` uses for the drag trio).
                let snap = ui
                    .scene_setup_panel
                    .resolve_scene_param(param_id)
                    .map(|(_, v)| v);
                let old_val = read_scene_node_param(project, &target, &addr).or(snap);
                if let Some(old_val) = old_val
                    && (old_val - *new_val).abs() > f32::EPSILON
                {
                    let mut live = SetGraphNodeParamCommand::new(
                        target.clone(),
                        addr.node_doc_id,
                        addr.param_id.clone(),
                        manifold_core::effect_graph_def::SerializedParamValue::Float { value: *new_val },
                        catalog_default.clone(),
                    )
                    .with_scope(addr.scope_path.clone());
                    live.execute(project);
                    let cmd = SetGraphNodeParamCommand::new(
                        target,
                        addr.node_doc_id,
                        addr.param_id.clone(),
                        manifold_core::effect_graph_def::SerializedParamValue::Float { value: *new_val },
                        catalog_default,
                    )
                    .with_scope(addr.scope_path.clone())
                    .with_previous(Some(manifold_core::effect_graph_def::SerializedParamValue::Float {
                        value: old_val,
                    }));
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
                return DispatchResult::handled();
            }
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let old_val = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(old_val) = old_val
                    && (old_val - *new_val).abs() > f32::EPSILON
                {
                    project.with_preset_graph_mut(&target, |inst| {
                        inst.set_base_param(param_id.as_ref(), *new_val);
                    });
                    let cmd = ChangeGraphParamCommand::new(target, param_id.clone(), old_val, *new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }

        // ── Effect modulation ──────────────────────────────────────
        PanelAction::DriverToggle(gpt, param_id) => {
            // BUG-249: scene rows redirect to their real exposed param
            // (materializing the exposure on first arm) — see
            // `resolve_mod_target`. Non-scene ids resolve exactly as before.
            let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, true,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            // Read the driver state off the SAME instance the command targets,
            // by target — never an ambient row index — so an editor-card driver
            // edit can't split (command -> watched instance, di -> another).
            let Some((existing, base_value)) = project.with_preset_graph_mut(&target, |inst| {
                let existing = inst
                    .drivers
                    .as_ref()
                    .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                    .map(|di| (di, inst.drivers.as_ref().unwrap()[di].enabled));
                let base_value = inst.get_param(param_id.as_ref());
                (existing, base_value)
            }) else {
                return DispatchResult::structural();
            };
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some((di, old)) = existing {
                    Box::new(ToggleDriverEnabledCommand::new(driver_target, di, old, !old))
                } else {
                    let driver = ParameterDriver {
                        param_id: param_id.clone(),
                        beat_division: BeatDivision::Quarter,
                        waveform: DriverWaveform::Sine,
                        enabled: true,
                        phase: 0.0,
                        base_value,
                        trim_min: 0.0,
                        trim_max: 1.0,
                        reversed: false,
                        free_period_beats: None,
                        legacy_param_index: None,
                        is_paused_by_user: false,
                    };
                    Box::new(AddDriverCommand::new(driver_target, driver))
                };
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioModToggle(gpt, param_id) => {
            let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, true,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            // Existing mod's enabled state, read off the resolved instance.
            let existing = project
                .with_preset_graph_mut(&target, |inst| {
                    inst.find_audio_mod(param_id.as_ref()).map(|a| a.enabled)
                })
                .flatten();
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some(old) = existing {
                    Box::new(ToggleAudioModEnabledCommand::new(
                        driver_target,
                        param_id.clone(),
                        old,
                        !old,
                    ))
                } else {
                    // Arm: assign the project's first audio send. No sends → inert
                    // (the audio button stays a no-op until the Audio Setup defines one).
                    let Some(send_id) = project.audio_setup.sends.first().map(|s| s.id.clone())
                    else {
                        return DispatchResult::structural();
                    };
                    let m = manifold_core::audio_mod::ParameterAudioMod::new(
                        param_id.clone(),
                        send_id,
                        manifold_core::AudioFeature::default(),
                    );
                    Box::new(AddAudioModCommand::new(driver_target, m))
                };
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioModSetSource(gpt, param_id, send_id, feature) => {
            let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, true,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            let old_source = project
                .with_preset_graph_mut(&target, |inst| {
                    inst.find_audio_mod(param_id.as_ref()).map(|a| a.source.clone())
                })
                .flatten();
            let driver_target = DriverTarget::from(&target);
            let new_source = manifold_core::audio_mod::AudioModSource {
                send_id: send_id.clone(),
                feature: crate::ui_translate::audio_feature_to_core(*feature),
            };
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some(old) = old_source {
                    Box::new(SetAudioModSourceCommand::new(
                        driver_target,
                        param_id.clone(),
                        old,
                        new_source,
                    ))
                } else {
                    let m = manifold_core::audio_mod::ParameterAudioMod::new(
                        param_id.clone(),
                        send_id.clone(),
                        crate::ui_translate::audio_feature_to_core(*feature),
                    );
                    Box::new(AddAudioModCommand::new(driver_target, m))
                };
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioModRemove(gpt, param_id) => {
            let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                Box::new(RemoveAudioModCommand::new(driver_target, param_id.clone()));
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioModSetInvert(gpt, param_id) => {
            // Flip the mod's invert flag in one undo step. Reads the current
            // shape, flips `invert`, commits old→new via the shape command.
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let old_shape = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
                if let Some(old_shape) = old_shape {
                    let mut new_shape = old_shape;
                    new_shape.invert = !old_shape.invert;
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModShapeCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_shape,
                            new_shape,
                        ));
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }

        PanelAction::AudioModSetRateOfChange(gpt, param_id) => {
            // Flip the mod's rate-of-change flag in one undo step — same shape
            // path as invert: read the current shape, flip `rate_of_change`,
            // commit old→new via the shape command.
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let old_shape = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
                if let Some(old_shape) = old_shape {
                    let mut new_shape = old_shape;
                    new_shape.rate_of_change = !old_shape.rate_of_change;
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModShapeCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_shape,
                            new_shape,
                        ));
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }

        PanelAction::AudioModShapeSnapshot(gpt, param_id) => {
            // Capture the pre-drag shape so the commit can record one undo step.
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                *audio_shape_snapshot = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
            }
            DispatchResult::handled()
        }
        PanelAction::AudioModShapeParamChanged(gpt, param_id, which, value) => {
            // Live edit (no undo entry per frame) — the handle tracks the cursor.
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let which = *which;
                let v = *value;
                graph_audio_mod_dual_edit(project, content_tx, &target, param_id.clone(), move |m| {
                    match which {
                        AudioShapeParam::Sensitivity => m.shape.sensitivity = v,
                        AudioShapeParam::Attack => m.shape.attack_ms = v,
                        AudioShapeParam::Release => m.shape.release_ms = v,
                    }
                });
            }
            DispatchResult::handled()
        }
        PanelAction::AudioModShapeCommit(gpt, param_id) => {
            // One undo step: snapshot (old) → current shape (new) via the shape command.
            if let Some(old_shape) = audio_shape_snapshot.take()
                && let Some((target, param_id)) = resolve_mod_target(
                    ui, project, content_tx, gpt, param_id, editor_target, effective_tab,
                    active_layer, selection, false,
                )
            {
                let param_id = &param_id;
                let new_shape = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
                if let Some(new_shape) = new_shape
                    && new_shape != old_shape
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModShapeCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_shape,
                            new_shape,
                        ));
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }

        // §9 U3: a trigger-gate row's Mode button — set `trigger_mode` on the
        // SAME `ParameterAudioMod` every other drawer edit targets (no
        // separate per-instance config, no separate command family).
        PanelAction::AudioModSetTriggerMode(gpt, param_id, mode_idx) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let old_mode = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.find_audio_mod(param_id.as_ref()).and_then(|m| m.trigger_mode)
                    })
                    .flatten();
                let new_mode = Some(match mode_idx {
                    1 => manifold_core::audio_trigger::TriggerFireMode::Transient,
                    2 => manifold_core::audio_trigger::TriggerFireMode::Both,
                    _ => manifold_core::audio_trigger::TriggerFireMode::ClipEdge,
                });
                if new_mode != old_mode {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModTriggerModeCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_mode,
                            new_mode,
                        ));
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }

        // PARAM_STEP_ACTIONS D8: the Action segmented row. Entering Step from
        // a non-Step action seeds `amount`/`wrap` from the param's own spec
        // (D2's UI-seeding default); re-clicking Step while already Step is a
        // no-op (keeps the user's dialed-in amount/wrap). Structural: the
        // Amount/Wrap/Mode rows and the collapsed "A"→"S"/"R" glyph all
        // depend on which action is armed.
        PanelAction::AudioModSetActionKind(gpt, param_id, kind_idx) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let (old_action, min, max, whole_numbers) = project
                    .with_preset_graph_mut(&target, |inst| {
                        let action = inst.find_audio_mod(param_id.as_ref()).map(|m| m.action);
                        let spec = inst.params.get(param_id.as_ref());
                        (
                            action,
                            spec.map(|p| p.spec.min).unwrap_or(0.0),
                            spec.map(|p| p.spec.max).unwrap_or(1.0),
                            spec.map(|p| p.whole_numbers()).unwrap_or(false),
                        )
                    })
                    .unwrap_or((None, 0.0, 1.0, false));
                if let Some(old_action) = old_action {
                    let new_action = match kind_idx {
                        1 => match old_action {
                            manifold_core::audio_mod::TriggerAction::Step { .. } => old_action,
                            _ => manifold_core::audio_mod::TriggerAction::Step {
                                amount: manifold_core::audio_mod::default_step_amount(
                                    min,
                                    max,
                                    whole_numbers,
                                ),
                                wrap: manifold_core::audio_mod::WrapMode::Wrap,
                            },
                        },
                        2 => manifold_core::audio_mod::TriggerAction::Random,
                        _ => manifold_core::audio_mod::TriggerAction::Continuous,
                    };
                    if new_action != old_action {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(SetAudioModActionCommand::new(
                                DriverTarget::from(&target),
                                param_id.clone(),
                                old_action,
                                new_action,
                            ));
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }

        PanelAction::AudioModStepAmountSnapshot(gpt, param_id) => {
            // Capture the pre-drag action so the commit can record one undo step.
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                *audio_action_snapshot = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.find_audio_mod(param_id.as_ref()).map(|m| m.action)
                    })
                    .flatten();
            }
            DispatchResult::handled()
        }
        PanelAction::AudioModStepAmountChanged(gpt, param_id, value) => {
            // Live edit (no undo entry per frame) — the handle tracks the cursor.
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let v = *value;
                graph_audio_mod_dual_edit(project, content_tx, &target, param_id.clone(), move |m| {
                    let wrap = match m.action {
                        manifold_core::audio_mod::TriggerAction::Step { wrap, .. } => wrap,
                        _ => manifold_core::audio_mod::WrapMode::Wrap,
                    };
                    m.action = manifold_core::audio_mod::TriggerAction::Step { amount: v, wrap };
                });
            }
            DispatchResult::handled()
        }
        PanelAction::AudioModStepAmountCommit(gpt, param_id) => {
            // One undo step: snapshot (old) → current action (new).
            if let Some(old_action) = audio_action_snapshot.take()
                && let Some((target, param_id)) = resolve_mod_target(
                    ui, project, content_tx, gpt, param_id, editor_target, effective_tab,
                    active_layer, selection, false,
                )
            {
                let param_id = &param_id;
                let new_action = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.find_audio_mod(param_id.as_ref()).map(|m| m.action)
                    })
                    .flatten();
                if let Some(new_action) = new_action
                    && new_action != old_action
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModActionCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_action,
                            new_action,
                        ));
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }

        // The Wrap segmented row — only meaningful while Action=Step; a stray
        // click while some other action is armed (shouldn't happen — the row
        // isn't built then) is a harmless no-op.
        PanelAction::AudioModSetWrap(gpt, param_id, wrap_idx) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let old_action = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.find_audio_mod(param_id.as_ref()).map(|m| m.action)
                    })
                    .flatten();
                if let Some(manifold_core::audio_mod::TriggerAction::Step { amount, .. }) = old_action {
                    let wrap = match wrap_idx {
                        1 => manifold_core::audio_mod::WrapMode::Bounce,
                        2 => manifold_core::audio_mod::WrapMode::Clamp,
                        _ => manifold_core::audio_mod::WrapMode::Wrap,
                    };
                    let new_action = manifold_core::audio_mod::TriggerAction::Step { amount, wrap };
                    if let Some(old_action) = old_action
                        && new_action != old_action
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(SetAudioModActionCommand::new(
                                DriverTarget::from(&target),
                                param_id.clone(),
                                old_action,
                                new_action,
                            ));
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }

        // ── Layer-owned clip triggers (P3b, AUDIO_SETUP_DOCK_AND_TRIGGER_
        // UNIFICATION_DESIGN.md D2/D5) — the inspector's AUDIO TRIGGERS
        // section. Addressed directly by `LayerId` + index (no
        // `resolve_graph_target`/`editor_target` involved — a clip trigger
        // isn't a graph param). Mutations route through P2's
        // Add/Remove/SetLayerClipTriggerCommand — whole-value-replace, same
        // shape as `SetAudioModTriggerModeCommand`.
        PanelAction::AudioTriggerSectionToggle => {
            ui.inspector.audio_trigger_section_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerRowExpandToggle(_layer_id, index) => {
            ui.inspector.audio_trigger_section_mut().toggle_row_expanded(*index);
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerAdd(layer_id) => {
            // One click = a firing trigger: enabled, listening to the first
            // send's kick cell (the dedicated ridge detector — the most
            // common thing a performer points a layer at), default shape,
            // 1b one-shot. The user hears it fire immediately and adjusts
            // from there. Inert until the Audio Setup dock defines a send
            // (mirrors `AudioModToggle`'s "arm" no-send case).
            if let Some(send_id) = project.audio_setup.sends.first().map(|s| s.id.clone()) {
                let mut trigger = manifold_core::audio_trigger::LayerClipTrigger::new(
                    manifold_core::audio_mod::AudioModSource {
                        send_id,
                        feature: manifold_core::AudioFeature::new(
                            manifold_core::AudioFeatureKind::Kick,
                            manifold_core::AudioBand::Low,
                        ),
                    },
                );
                trigger.enabled = true;
                let new_index = project
                    .timeline
                    .find_layer_by_id_mut(layer_id)
                    .map(|(_, l)| l.clip_triggers.len());
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                    Box::new(AddLayerClipTriggerCommand::new(layer_id.clone(), trigger));
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                // Open the new row's drawer so its (now minimal) tuning is
                // immediately visible.
                if let Some(index) = new_index {
                    ui.inspector.audio_trigger_section_mut().expand_row(index);
                }
            }
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerRemove(layer_id, index) => {
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                Box::new(RemoveLayerClipTriggerCommand::new(layer_id.clone(), *index));
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerEnabledToggle(layer_id, index) => {
            let old = project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index).cloned());
            if let Some(old) = old {
                let mut new = old.clone();
                new.enabled = !old.enabled;
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(
                    SetLayerClipTriggerCommand::new(layer_id.clone(), *index, old, new),
                );
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerSetSource(layer_id, index, send_id, feature) => {
            let old = project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index).cloned());
            if let Some(old) = old {
                let mut new = old.clone();
                new.source = manifold_core::audio_mod::AudioModSource {
                    send_id: send_id.clone(),
                    feature: crate::ui_translate::audio_feature_to_core(*feature),
                };
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(
                    SetLayerClipTriggerCommand::new(layer_id.clone(), *index, old, new),
                );
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerShapeSnapshot(layer_id, index) => {
            // Reuses `audio_shape_snapshot` (the param-mod shaping-slider
            // slot) rather than a dedicated field: only one drawer slider
            // can be mid-drag at a time (single-threaded UI dispatch), so
            // the snapshot/commit pair for this target never overlaps a
            // param-mod drag's own use of the same slot.
            *audio_shape_snapshot = project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index))
                .map(|t| t.shape);
            DispatchResult::handled()
        }
        PanelAction::AudioTriggerShapeParamChanged(layer_id, index, which, value) => {
            let which = *which;
            let v = *value;
            clip_trigger_shape_dual_edit(project, content_tx, layer_id, *index, move |shape| {
                match which {
                    AudioShapeParam::Sensitivity => shape.sensitivity = v,
                    AudioShapeParam::Attack => shape.attack_ms = v,
                    AudioShapeParam::Release => shape.release_ms = v,
                }
            });
            DispatchResult::handled()
        }
        PanelAction::AudioTriggerShapeCommit(layer_id, index) => {
            if let Some(old_shape) = audio_shape_snapshot.take() {
                let current = project
                    .timeline
                    .find_layer_by_id_mut(layer_id)
                    .and_then(|(_, l)| l.clip_triggers.get(*index).cloned());
                if let Some(current) = current
                    && current.shape != old_shape
                {
                    let mut old = current.clone();
                    old.shape = old_shape;
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(
                        SetLayerClipTriggerCommand::new(layer_id.clone(), *index, old, current),
                    );
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::AudioTriggerSetLength(layer_id, index, beats) => {
            let old = project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index).cloned());
            if let Some(old) = old {
                let mut new = old.clone();
                new.one_shot_beats = manifold_core::Beats(*beats as f64);
                if new != old {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(
                        SetLayerClipTriggerCommand::new(layer_id.clone(), *index, old, new),
                    );
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }

        // ── Audio Setup (project-level send routing) ──────────────
        PanelAction::AudioSetDevice(device) => {
            let old = project.audio_setup.device.clone();
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioInputDeviceCommand::new(
                    old,
                    device.as_ref().map(crate::ui_translate::audio_device_ref_to_core),
                )),
            )
        }
        PanelAction::AudioAddSend => {
            let send = manifold_core::audio_setup::AudioSend::new(format!(
                "Audio {}",
                project.audio_setup.sends.len() + 1
            ));
            audio_setup_command(project, content_tx, Box::new(AddAudioSendCommand::new(send)))
        }
        PanelAction::AudioRemoveSend(id) => audio_setup_command(
            project,
            content_tx,
            Box::new(RemoveAudioSendCommand::new(id.clone())),
        ),
        PanelAction::AudioRenameSend(id, label) => {
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.label.clone())
                .unwrap_or_default();
            audio_setup_command(
                project,
                content_tx,
                Box::new(RenameAudioSendCommand::new(id.clone(), old, label.clone())),
            )
        }
        PanelAction::AudioSetSendChannels(id, ch) => {
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.channels.clone())
                .unwrap_or_default();
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendChannelsCommand::new(id.clone(), old, ch.clone())),
            )
        }
        // `AudioSendStereoToggle` is deleted (§7.2 item 6, P8, 2026-07-11) —
        // the channel dropdown now carries any channel vec directly via
        // `AudioSetSendChannels` above; mono falls out of picking one channel.
        PanelAction::AudioSendGainStep(id, delta_db) => {
            // The project is the source of truth: read current gain, apply the
            // delta, clamp to a sensible trim range, commit old→new. Capture
            // restart is avoided — the worker reads gain live (AudioModRuntime).
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.gain_db)
                .unwrap_or(0.0);
            let new = (old + delta_db).clamp(AUDIO_SEND_GAIN_MIN_DB, AUDIO_SEND_GAIN_MAX_DB);
            if (new - old).abs() < f32::EPSILON {
                return DispatchResult::structural();
            }
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendGainCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioSendGainDragBegin(id) => {
            // Snapshot the pre-drag gain so the commit records one undo step —
            // the `AudioCrossoverDragBegin` pattern, per-send (D7).
            *audio_send_gain_drag_snapshot = Some(
                project
                    .audio_setup
                    .find_send(id)
                    .map(|s| s.gain_db)
                    .unwrap_or(0.0),
            );
            DispatchResult::handled()
        }
        PanelAction::AudioSendGainDragChanged(id, db) => {
            // Live edit (no per-frame undo): clamp to the stepper's trim range,
            // then apply to the local project and the content thread so the
            // label + `GainBank` track the cursor — no capture restart.
            let clamped = db.clamp(AUDIO_SEND_GAIN_MIN_DB, AUDIO_SEND_GAIN_MAX_DB);
            if let Some(s) = project.audio_setup.find_send_mut(id) {
                s.gain_db = clamped;
            }
            let id = id.clone();
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    if let Some(s) = p.audio_setup.find_send_mut(&id) {
                        s.gain_db = clamped;
                    }
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::AudioSendGainDragCommit(id) => {
            // One undo step: snapshot (old) → current gain (new).
            if let Some(old) = audio_send_gain_drag_snapshot.take() {
                let new = project.audio_setup.find_send(id).map(|s| s.gain_db).unwrap_or(old);
                if (new - old).abs() > f32::EPSILON {
                    return audio_setup_command(
                        project,
                        content_tx,
                        Box::new(SetAudioSendGainCommand::new(id.clone(), old, new)),
                    );
                }
            }
            DispatchResult::handled()
        }
        // P4 (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D8, audio-dock sibling):
        // the type-in commit — ONE undo step, no clamp. Unlike
        // `AudioSendGainDragChanged`'s live-drag path, a typed value is free
        // to exceed `AUDIO_SEND_GAIN_MIN_DB`/`MAX_DB` (PARAM_RANGE_CONTRACT
        // P1: those are the stepper's display travel, not a hard limit).
        PanelAction::AudioSendGainSetTyped(id, new_db) => {
            let old = project.audio_setup.find_send(id).map(|s| s.gain_db).unwrap_or(0.0);
            if (new_db - old).abs() < f32::EPSILON {
                return DispatchResult::structural();
            }
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendGainCommand::new(id.clone(), old, *new_db)),
            )
        }
        PanelAction::AudioSendFloorStep(id, delta_db) => {
            // Pre-analysis squelch (dB). Off is a sentinel below the usable range:
            // stepping up from off engages the gate at its bottom; stepping below
            // the bottom turns it back off. Applied live (AudioModRuntime).
            const FLOOR_MIN_DB: f32 = -100.0;
            const FLOOR_MAX_DB: f32 = -6.0;
            let off = manifold_core::audio_setup::FLOOR_DB_OFF;
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.floor_db)
                .unwrap_or(off);
            let new = if old <= off {
                if *delta_db > 0.0 { FLOOR_MIN_DB } else { off }
            } else {
                let v = old + *delta_db;
                if v < FLOOR_MIN_DB { off } else { v.min(FLOOR_MAX_DB) }
            };
            if (new - old).abs() < f32::EPSILON {
                return DispatchResult::structural();
            }
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendFloorCommand::new(id.clone(), old, new)),
            )
        }
        // The Audio Setup Triggers matrix's dispatch arms (AudioTriggerToggled,
        // AudioTriggerSensitivityStep, AudioSendSensitivityDragBegin/Changed/
        // Commit, AudioTriggerLengthStep, AudioTriggerSetLayer,
        // AudioTriggerLayerClicked) are deleted with the matrix (P3, D2). Clip
        // triggers are authored on the layer only (`LayerClipTrigger`, P2).
        // `AudioSendAddLayerClicked` (Inputs section "+ Layer") is deleted
        // with the section's authoring (§7.2 item 7, P8, 2026-07-11).
        PanelAction::AudioCrossoverDragBegin => {
            // Snapshot the pre-drag crossovers so the commit records one undo step.
            *audio_crossover_snapshot =
                Some((project.audio_setup.low_hz, project.audio_setup.mid_hz));
            DispatchResult::handled()
        }
        PanelAction::AudioCrossoverChanged(band, hz) => {
            // Live edit (no per-frame undo): clamp the dragged line against the
            // other and the band edges, then apply to the local project and the
            // content thread so the divider + analysis bands track the cursor.
            let dragging_low = matches!(band, manifold_ui::BandDivider::Low);
            let (cur_low, cur_mid) = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
            let (low, mid) = if dragging_low {
                manifold_core::audio_setup::AudioSetup::clamp_crossovers(*hz, cur_mid, true)
            } else {
                manifold_core::audio_setup::AudioSetup::clamp_crossovers(cur_low, *hz, false)
            };
            project.audio_setup.low_hz = low;
            project.audio_setup.mid_hz = mid;
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    p.audio_setup.low_hz = low;
                    p.audio_setup.mid_hz = mid;
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::AudioCrossoverCommit => {
            // One undo step: snapshot (old) → current crossovers (new).
            if let Some(old) = audio_crossover_snapshot.take() {
                let new = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
                if new != old {
                    return audio_setup_command(
                        project,
                        content_tx,
                        Box::new(SetAudioCrossoversCommand::new(old, new)),
                    );
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EnvelopeToggle(gpt, param_id) => {
            // Envelope-home unification: the envelope rides on the resolved
            // instance (keyed by param_id) for effects and generators alike.
            // Toggle the existing one's `enabled`, or create a fresh enabled
            // envelope. Effects are clip-timed, so only layer effects get
            // envelopes (master/clip have no trigger — the button is inert
            // there); generators are layer-scoped, always permitted.
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, true,
            ) {
                let param_id = &param_id;
                let env_allowed = match &target {
                    manifold_core::GraphTarget::Generator(_) => true,
                    manifold_core::GraphTarget::Effect(_) => {
                        matches!(effective_tab, InspectorTab::Layer)
                    }
                };
                if env_allowed {
                    let pid = param_id.clone();
                    let toggle = move |p: &mut Project| {
                        p.with_preset_graph_mut(&target, |inst| {
                            let envs = inst.envelopes.get_or_insert_with(Vec::new);
                            if let Some(idx) = envs.iter().position(|e| e.param_id == pid) {
                                envs[idx].enabled = !envs[idx].enabled;
                            } else {
                                envs.push(ParamEnvelope::new(pid.clone()));
                            }
                        });
                    };
                    toggle(project);
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::MutateProject(Box::new(toggle)),
                    );
                }
            }
            DispatchResult::structural()
        }
        PanelAction::DriverConfig(gpt, param_id, cfg) => {
            let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            let driver_target = DriverTarget::from(&target);
            // Read the driver's current config off the same instance the
            // command targets (by GraphTarget), so an editor-card edit can't
            // split command vs row index.
            let info = project
                .with_preset_graph_mut(&target, |inst| {
                    inst.drivers
                        .as_ref()
                        .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                        .map(|di| {
                            let d = &inst.drivers.as_ref().unwrap()[di];
                            (di, d.beat_division, d.waveform, d.reversed, d.free_period_beats)
                        })
                })
                .flatten();
            if let Some((di, beat_division, waveform, reversed, free)) = info {
                type BoxedCmd = Box<dyn manifold_editing::command::Command + Send>;
                // The feel segment sets the division's modifier from its base; a
                // base without a dotted/triplet variant (e.g. 1/32) keeps the base.
                let base = beat_division.base_division();
                let cmd: Option<BoxedCmd> = match cfg {
                    DriverConfigAction::BeatDiv(idx) => BeatDivision::from_button_index(*idx)
                        .map(|new_div| {
                            Box::new(ChangeDriverBeatDivCommand::new(
                                driver_target,
                                di,
                                beat_division,
                                new_div,
                                free,
                            )) as BoxedCmd
                        }),
                    DriverConfigAction::Wave(idx) => DriverWaveform::from_index(*idx).map(|new_wave| {
                        Box::new(ChangeDriverWaveformCommand::new(
                            driver_target,
                            di,
                            waveform,
                            new_wave,
                        )) as BoxedCmd
                    }),
                    DriverConfigAction::Straight => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base,
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Dotted => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base.toggle_dotted().unwrap_or(base),
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Triplet => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base.toggle_triplet().unwrap_or(base),
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Invert => Some(Box::new(ToggleDriverReversedCommand::new(
                        driver_target,
                        di,
                        reversed,
                        !reversed,
                    )) as BoxedCmd),
                    DriverConfigAction::SetFreePeriod(p) => {
                        Some(Box::new(SetDriverFreePeriodCommand::new(
                            driver_target,
                            di,
                            free,
                            Some(*p),
                        )) as BoxedCmd)
                    }
                };
                if let Some(mut boxed) = cmd {
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        // Live trim-range edit for driver / Ableton / audio — one arm,
        // `TrimKind` selects the backing store. Each kind keeps the exact
        // edit it had before the unification (driver dual-edit, audio dual-edit,
        // Ableton mapping local + content-sync).
        PanelAction::TrimChanged(kind, gpt, param_id, min, max) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let mn = *min;
                let mx = *max;
                // Guard the in-flight range against a concurrent snapshot swap
                // (BUG-246): restored via ActiveInspectorDrag::Trim::apply.
                // Ableton needs its resolved mapping target; driver/audio don't.
                let ableton_target = matches!(kind, TrimKind::Ableton)
                    .then(|| {
                        ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
                    })
                    .flatten();
                *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::Trim {
                    kind: *kind,
                    target: target.clone(),
                    ableton_target,
                    param_id: param_id.clone(),
                    min: mn,
                    max: mx,
                });
                match kind {
                    TrimKind::Driver => {
                        graph_driver_dual_edit(project, content_tx, &target, param_id.clone(), move |d| {
                            d.trim_min = mn;
                            d.trim_max = mx;
                        });
                    }
                    TrimKind::Audio => {
                        graph_audio_mod_dual_edit(project, content_tx, &target, param_id.clone(), move |m| {
                            m.shape.range_min = mn;
                            m.shape.range_max = mx;
                        });
                    }
                    TrimKind::Ableton => {
                        if let Some(mapping_target) =
                            ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
                        {
                            // Local edit + content sync both route through the
                            // shared locate-fork, so they can't split (effects
                            // locate by effect_type — first match — on both
                            // sides now).
                            if let Some(ms) = project
                                .ableton_param_mappings_mut(&mapping_target)
                                .and_then(|opt| opt.as_mut())
                                && let Some(m) = ms.iter_mut().find(|m| m.param_id == *param_id)
                            {
                                m.range_min = mn;
                                m.range_max = mx;
                            }
                            let mt = mapping_target.clone();
                            let pid = param_id.clone();
                            ContentCommand::send(
                                content_tx,
                                ContentCommand::MutateProject(Box::new(move |p| {
                                    if let Some(ms) =
                                        p.ableton_param_mappings_mut(&mt).and_then(|opt| opt.as_mut())
                                        && let Some(m) = ms.iter_mut().find(|m| m.param_id == pid)
                                    {
                                        m.range_min = mn;
                                        m.range_max = mx;
                                    }
                                })),
                            );
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::TargetChanged(gpt, param_id, norm) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let n = *norm;
                graph_env_dual_edit(project, content_tx, &target, param_id.clone(), move |env| {
                    env.target_normalized = n;
                });
            }
            DispatchResult::handled()
        }
        PanelAction::EnvDecayChanged(gpt, param_id, decay) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let d = *decay;
                graph_env_dual_edit(project, content_tx, &target, param_id.clone(), move |env| {
                    env.decay_beats = d;
                });
            }
            DispatchResult::handled()
        }

        // ── Modulation undo: snapshot/commit ────────────────────────
        // Snapshot the kind's pre-drag range into the shared `trim_snapshot`.
        // Only one trim handle drags at a time, so one slot serves all kinds.
        PanelAction::TrimSnapshot(kind, gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let range = match kind {
                    TrimKind::Driver => project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.drivers
                                .as_ref()
                                .and_then(|ds| ds.iter().find(|d| d.param_id == *param_id))
                                .map(|d| (d.trim_min, d.trim_max))
                        })
                        .flatten(),
                    TrimKind::Audio => project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.audio_mods
                                .as_ref()
                                .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                                .map(|m| (m.shape.range_min, m.shape.range_max))
                        })
                        .flatten(),
                    TrimKind::Ableton => {
                        ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
                            .and_then(|mt| {
                                project
                                    .ableton_param_mappings(&mt)
                                    .and_then(|opt| opt.as_ref())
                                    .and_then(|ms| ms.iter().find(|m| m.param_id == *param_id))
                                    .map(|m| (m.range_min, m.range_max))
                            })
                    }
                };
                if let Some(range) = range {
                    *trim_snapshot = Some(range);
                }
            }
            DispatchResult::handled()
        }
        PanelAction::TrimCommit(kind, gpt, param_id) => {
            match kind {
                TrimKind::Driver => {
                    if let Some((old_min, old_max)) = trim_snapshot.take()
                        && let Some(target) = resolve_graph_target(
                            gpt, editor_target, effective_tab, active_layer, selection, project,
                        )
                    {
                        let info = project
                            .with_preset_graph_mut(&target, |inst| {
                                inst.drivers
                                    .as_ref()
                                    .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                                    .map(|di| {
                                        let d = &inst.drivers.as_ref().unwrap()[di];
                                        (di, d.trim_min, d.trim_max)
                                    })
                            })
                            .flatten();
                        if let Some((di, new_min, new_max)) = info
                            && ((old_min - new_min).abs() > f32::EPSILON
                                || (old_max - new_max).abs() > f32::EPSILON)
                        {
                            let cmd = ChangeTrimCommand::new(
                                DriverTarget::from(&target),
                                di,
                                old_min,
                                old_max,
                                new_min,
                                new_max,
                            );
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
                TrimKind::Audio => {
                    if let Some((old_min, old_max)) = trim_snapshot.take()
                        && let Some(target) = resolve_graph_target(
                            gpt, editor_target, effective_tab, active_layer, selection, project,
                        )
                    {
                        let new_shape = project
                            .with_preset_graph_mut(&target, |inst| {
                                inst.audio_mods
                                    .as_ref()
                                    .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                                    .map(|m| m.shape)
                            })
                            .flatten();
                        if let Some(new_shape) = new_shape
                            && ((old_min - new_shape.range_min).abs() > f32::EPSILON
                                || (old_max - new_shape.range_max).abs() > f32::EPSILON)
                        {
                            let mut old_shape = new_shape;
                            old_shape.range_min = old_min;
                            old_shape.range_max = old_max;
                            let cmd = SetAudioModShapeCommand::new(
                                DriverTarget::from(&target),
                                param_id.clone(),
                                old_shape,
                                new_shape,
                            );
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
                // Ableton trim now records undo like driver/audio: the live
                // edit already landed via `TrimChanged`, so the command just
                // re-applies the same range and captures the pre-drag range for
                // undo. One step per drag (snapshot on grab, commit on release).
                TrimKind::Ableton => {
                    if let Some((old_min, old_max)) = trim_snapshot.take()
                        && let Some(target) = resolve_graph_target(
                            gpt, editor_target, effective_tab, active_layer, selection, project,
                        )
                        && let Some(mt) = ableton_mapping_target(
                            &target, effective_tab, active_layer, project, param_id,
                        )
                    {
                        let new = project
                            .ableton_param_mappings(&mt)
                            .and_then(|opt| opt.as_ref())
                            .and_then(|ms| ms.iter().find(|m| m.param_id == *param_id))
                            .map(|m| (m.range_min, m.range_max));
                        if let Some((new_min, new_max)) = new
                            && ((old_min - new_min).abs() > f32::EPSILON
                                || (old_max - new_max).abs() > f32::EPSILON)
                        {
                            let cmd = ChangeAbletonTrimCommand::new(
                                mt, old_min, old_max, new_min, new_max,
                            );
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::TargetSnapshot(gpt, param_id) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let t = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.envelopes
                            .as_ref()
                            .and_then(|es| es.iter().find(|e| e.param_id == *param_id))
                            .map(|e| e.target_normalized)
                    })
                    .flatten();
                if let Some(t) = t {
                    *target_snapshot = Some(t);
                }
            }
            DispatchResult::handled()
        }
        PanelAction::TargetCommit(gpt, param_id) => {
            if let Some(old_target) = target_snapshot.take()
                && let Some((target, param_id)) = resolve_mod_target(
                    ui, project, content_tx, gpt, param_id, editor_target, effective_tab,
                    active_layer, selection, false,
                )
            {
                let param_id = &param_id;
                let info = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.envelopes
                            .as_ref()
                            .and_then(|es| es.iter().position(|e| e.param_id == *param_id))
                            .map(|idx| (idx, inst.envelopes.as_ref().unwrap()[idx].target_normalized))
                    })
                    .flatten();
                if let Some((env_idx, new_t)) = info
                    && (old_target - new_t).abs() > f32::EPSILON
                {
                    let cmd =
                        ChangeEnvelopeTargetCommand::new(target, env_idx, old_target, new_t);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::EnvDecaySnapshot(gpt, param_id) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ui, project, content_tx, gpt, param_id, editor_target, effective_tab, active_layer,
                selection, false,
            ) {
                let param_id = &param_id;
                let d = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.envelopes
                            .as_ref()
                            .and_then(|es| es.iter().find(|e| e.param_id == *param_id))
                            .map(|e| e.decay_beats)
                    })
                    .flatten();
                if let Some(d) = d {
                    *decay_snapshot = Some(d);
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EnvDecayCommit(gpt, param_id) => {
            if let Some(old_decay) = decay_snapshot.take()
                && let Some((target, param_id)) = resolve_mod_target(
                    ui, project, content_tx, gpt, param_id, editor_target, effective_tab,
                    active_layer, selection, false,
                )
            {
                let param_id = &param_id;
                let info = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.envelopes
                            .as_ref()
                            .and_then(|es| es.iter().position(|e| e.param_id == *param_id))
                            .map(|idx| (idx, inst.envelopes.as_ref().unwrap()[idx].decay_beats))
                    })
                    .flatten();
                if let Some((env_idx, new_d)) = info
                    && (old_decay - new_d).abs() > f32::EPSILON
                {
                    let cmd =
                        ChangeEnvelopeDecayCommand::new(target, env_idx, old_decay, new_d);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Effect management ──────────────────────────────────────
        PanelAction::AddEffectClicked(_tab) => DispatchResult::handled(),
        PanelAction::BrowserSearchClicked => DispatchResult::handled(),
        PanelAction::RemoveEffect(fx_idx) => {
            let tab = effective_tab;
            let (effects_ref, target) = resolve_effects_read(tab, project, active_layer, selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*fx_idx)
            {
                let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectReorder(from_idx, to_idx) => {
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, project);
            let cmd = ReorderEffectCommand::new(target, *from_idx, *to_idx);
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::structural()
        }
        // `PanelAction::ToggleNodeParamExpose` is handled in
        // `app_render.rs` alongside the other graph commands so it can
        // access `watched_graph_target` + `watched_catalog_default`
        // directly. No fork on Effect vs Generator at the dispatch
        // layer — the command itself handles both.
        PanelAction::EffectReorderGroup(source_indices, target_idx) => {
            // Multi-select reorder: move a group of effects to the target position.
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, project);
            let (effects_mut, _target) = resolve_effects_mut(tab, project, active_layer, selection);
            if let Some(effects) = effects_mut {
                // Snapshot before
                let old_effects = effects.clone();

                // Remove selected effects in reverse order (preserving relative order)
                let mut moving: Vec<(usize, PresetInstance)> = Vec::new();
                let mut sorted_sources = source_indices.clone();
                sorted_sources.sort_unstable();
                for &idx in sorted_sources.iter().rev() {
                    if idx < effects.len() {
                        moving.push((idx, effects.remove(idx)));
                    }
                }
                moving.reverse(); // Restore original relative order

                // Compute adjusted insertion point (account for removed items before target)
                let removed_before = sorted_sources.iter().filter(|&&i| i < *target_idx).count();
                let insert_at = target_idx.saturating_sub(removed_before).min(effects.len());

                // Insert the group at the target position
                for (offset, (_, fx)) in moving.into_iter().enumerate() {
                    let pos = (insert_at + offset).min(effects.len());
                    effects.insert(pos, fx);
                }

                // Snapshot after and create undoable command
                let new_effects = effects.clone();
                let cmd = ReorderEffectGroupCommand::new(target, old_effects, new_effects);
                // State already applied — send for undo stack only (don't re-execute)
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::structural()
        }

        // ── Generator card actions ─────────────────────────────────
        PanelAction::GenStringParamClicked(_) | PanelAction::GenStringParamDropdownClicked(_) => {
            // Intercepted in app_render.rs to open text input / dropdown.
            DispatchResult::handled()
        }
        PanelAction::GenStringParamSelected(sp_idx, selected_value) => {
            // A dropdown string param was selected (e.g. font family).
            // Commit it as a SetClipStringParamCommand.
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
            {
                let gen_type = layer.generator_type();
                if let Some(def) = manifold_core::preset_definition_registry::try_get(gen_type)
                    && let Some(sp_def) = def.string_param_defs.get(*sp_idx)
                {
                    let key = sp_def.key.to_string();
                    let new_value: Option<String> = if selected_value.is_empty() {
                        None
                    } else {
                        Some(selected_value.clone())
                    };

                    // Find clip: selected clip on this layer, or first clip
                    let clip = selection
                        .primary_selected_clip_id
                        .as_ref()
                        .and_then(|sel_id| layer.clips.iter().find(|c| c.id == *sel_id))
                        .or_else(|| layer.clips.first());
                    if let Some(c) = clip {
                        let old_value = c.string_params.as_ref().and_then(|m| m.get(&key)).cloned();
                        if old_value != new_value {
                            let clip_id = c.id.clone();
                            let cmd =
                                manifold_editing::commands::clip::SetClipStringParamCommand::new(
                                    clip_id, key, old_value, new_value,
                                );
                            ContentCommand::send(
                                content_tx,
                                ContentCommand::Execute(Box::new(cmd)),
                            );
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenCollapseToggle => {
            if let Some(gp) = ui.inspector.gen_params_mut() {
                let new_val = !gp.is_collapsed();
                gp.set_collapsed(new_val);
            }
            DispatchResult::structural()
        }
        PanelAction::GenCardClicked => {
            // Select the generator card (blue highlight border), deselect effect cards
            if let Some(gp) = ui.inspector.gen_params_mut() {
                gp.update_selection_visual(&mut ui.tree, true);
            }
            // Deselect all effect cards
            ui.inspector.clear_effect_selection(&mut ui.tree);
            DispatchResult::handled()
        }
        PanelAction::CardRightClicked(_) => {
            // Handled by UIRoot::try_open_dropdown (opens the card context menu)
            // — should not reach dispatch.
            DispatchResult::handled()
        }
        PanelAction::CopyGenerator => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                && let Some(gp) = layer.gen_params()
            {
                ui.gen_clipboard.copy_from(gp);
            }
            DispatchResult::handled()
        }
        PanelAction::PasteGenerator => {
            if let Some(snapshot) = ui.gen_clipboard.get_paste_snapshot() {
                let layer_idx = super::resolve_active_layer_index(active_layer, project);
                if let Some(layer_idx) = layer_idx
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params()
                {
                    let layer_id = layer.layer_id.clone();
                    let old_type = gp.generator_type().clone();
                    let old_params = gp.snapshot_params();
                    let old_drivers = gp.snapshot_drivers();
                    let old_envelopes = gp.snapshot_envelopes();

                    let cmd = PasteGeneratorCommand::new(
                        layer_id,
                        old_type,
                        old_params,
                        old_drivers,
                        old_envelopes,
                        snapshot.generator_type,
                        snapshot.param_values,
                        snapshot.drivers,
                        snapshot.envelopes,
                    );
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::MakePresetUnique(gpt) => {
            // Fork the targeted preset (effect OR generator) into a
            // project-embedded copy and retarget the instance to it. One path
            // for both kinds: resolve the GraphTarget, take its source def
            // (diverged per-instance graph else catalog canonical), fork via
            // the shared command keyed off `target.preset_kind()`.
            use manifold_editing::commands::preset::ForkPresetCommand;
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some((source_def, _)) = preset_source_def(&target, project)
            {
                let cmd = ForkPresetCommand::new(target.clone(), target.preset_kind(), source_def);
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::ExportPreset(gpt) => {
            // Export the targeted preset's graph to a .json via a native save
            // dialog. Source def is the diverged per-instance graph else the
            // catalog canonical; the preset id is the filename stem.
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some((def, preset_id)) = preset_source_def(&target, project)
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("MANIFOLD Preset", &["json"])
                    .set_file_name(format!("{}.json", preset_id.as_str()))
                    .save_file()
                && let Err(e) = manifold_io::preset_file::export_preset(&def, &path)
            {
                log::error!("[preset] export failed: {e}");
            }
            DispatchResult::handled()
        }
        PanelAction::ImportPreset(gpt) => {
            // Import a .json preset and retarget the targeted instance to it
            // (registered as a project-embedded preset via the shared fork
            // command, so it rides undo + the overlay refresh).
            use manifold_editing::commands::preset::ForkPresetCommand;
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some(path) = rfd::FileDialog::new()
                .add_filter("MANIFOLD Preset", &["json"])
                .pick_file()
            {
                match manifold_io::preset_file::import_preset(&path) {
                    Ok(def) => {
                        let cmd =
                            ForkPresetCommand::importing(target.clone(), target.preset_kind(), def);
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                    Err(e) => log::error!("[preset] import failed: {e}"),
                }
            }
            DispatchResult::structural()
        }
        PanelAction::SaveToLibrary(gpt) | PanelAction::SaveToProject(gpt) => {
            // Library doors (PRESET_LIBRARY_DESIGN D4): resolve the target's
            // current effective def (same `preset_source_def` resolution as
            // Make Unique / Export) and hand it back on `DispatchResult` for
            // the caller to open the shared name-prompt text-input session
            // with — this function has no `TextInputState` access (it's
            // UI-thread overlay state, not routed here), so the prompt itself
            // opens one level up.
            let mut result = DispatchResult::handled();
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some((def, _)) = preset_source_def(&target, project)
            {
                let destination = if matches!(action, PanelAction::SaveToLibrary(_)) {
                    crate::text_input::SavePresetDestination::Library
                } else {
                    crate::text_input::SavePresetDestination::Project
                };
                result.begin_save_preset = Some((target.preset_kind(), def, destination));
            }
            result
        }
        PanelAction::RevertToLibrary(gpt) => {
            // PRESET_LIBRARY_DESIGN D3/P4: clear the per-instance graph
            // override, undoable — but ONLY if the tracked library id still
            // resolves in the catalog. The resolution check happens HERE
            // (app/renderer-aware) rather than inside the command itself:
            // `manifold-editing` cannot depend on `manifold-renderer`
            // (`manifold-playback` already depends on `manifold-editing`,
            // and `manifold-renderer` depends on `manifold-playback` — the
            // reverse dependency would cycle), so the fact is resolved once
            // here and baked into the command, mirroring how
            // `ForkPresetCommand` is handed an already-resolved `source_def`
            // rather than looking the catalog up inside `Command::execute`.
            use manifold_editing::commands::preset::RevertToLibraryCommand;
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some(preset_id) = project.instance_preset_id(&target)
            {
                let resolves = manifold_renderer::node_graph::loaded_preset_view_by_id(&preset_id)
                    .is_some();
                let cmd = RevertToLibraryCommand::new(target, resolves);
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::PushToLibrary(gpt) => {
            // Push to Library (D3, P4): overwrite the targeted preset's
            // tracked user-library file with its current (diverged)
            // definition in place — no name prompt (id/filename never
            // change). A factory/stock id has no user file to overwrite;
            // fall back to the same Save-to-Library-as-new prompt the
            // `SaveToLibrary` action opens, via `begin_save_preset` (this
            // function has no `TextInputState` access — see the comment on
            // the `SaveToLibrary`/`SaveToProject` arm above).
            let mut result = DispatchResult::handled();
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some((def, preset_id)) = preset_source_def(&target, project)
            {
                let kind = target.preset_kind();
                let lib = crate::user_library::UserLibrary::new();
                if lib.is_user_entry(kind, &preset_id) {
                    if let Err(e) = lib.push(kind, &preset_id, &def) {
                        log::error!("[preset] push to library failed: {e}");
                    }
                } else {
                    result.begin_save_preset =
                        Some((kind, def, crate::text_input::SavePresetDestination::Library));
                }
            }
            result
        }

        // ── Browser: sources, badges, management (PRESET_LIBRARY_DESIGN P5) ──
        // `BrowserCellRightClicked` opens its menu entirely inside
        // `UIRoot::try_open_dropdown` — this arm only keeps the match
        // exhaustive (same pattern as `CardRightClicked` above).
        PanelAction::BrowserCellRightClicked(..) => DispatchResult::handled(),
        PanelAction::BrowserRenamePresetClicked(mode, type_id, source) => {
            use manifold_ui::panels::picker_core::Source;

            let kind = browser_mode_to_kind(*mode);
            let id = manifold_core::PresetTypeId::from_string(type_id.clone());
            let initial_name = match source {
                Source::MyLibrary => {
                    manifold_core::preset_type_registry::available_of_kind(kind)
                        .iter()
                        .find(|r| r.id.as_str() == type_id.as_str())
                        .map(|r| r.display_name.to_string())
                }
                Source::Project => project
                    .embedded_preset(&id)
                    .and_then(|ep| ep.def.preset_metadata.as_ref())
                    .map(|m| m.display_name.clone()),
                Source::Factory => None, // unreachable — the menu never offers Rename for Factory
            }
            .unwrap_or_else(|| type_id.clone());

            let mut result = DispatchResult::handled();
            result.begin_rename_preset = Some((kind, id, *source, initial_name));
            ui.browser_popup.close();
            result
        }
        PanelAction::BrowserDuplicatePresetClicked(mode, type_id) => {
            // My Library only — the menu never offers Duplicate for Project.
            let kind = browser_mode_to_kind(*mode);
            let id = manifold_core::PresetTypeId::from_string(type_id.clone());
            let lib = crate::user_library::UserLibrary::new();
            match lib.duplicate(kind, &id) {
                Ok(new_id) => log::info!("[preset] duplicated '{}' as '{}'", id.as_str(), new_id.as_str()),
                Err(e) => log::error!("[preset] duplicate failed: {e}"),
            }
            ui.browser_popup.close();
            DispatchResult::handled()
        }
        PanelAction::BrowserDeletePresetClicked(mode, type_id, source) => {
            use manifold_ui::panels::picker_core::Source;

            let kind = browser_mode_to_kind(*mode);
            let id = manifold_core::PresetTypeId::from_string(type_id.clone());
            let (place, undo_note) = match source {
                Source::MyLibrary => ("your library", "This can't be undone."),
                Source::Project => ("this project", "Undo (\u{2318}Z) restores it."),
                Source::Factory => return DispatchResult::handled(), // unreachable
            };
            let confirmed = crate::alerts::confirm(
                "Delete preset",
                &format!("Delete \"{type_id}\" from {place}?\n\n{undo_note}"),
            );
            if !confirmed {
                return DispatchResult::handled();
            }
            match source {
                Source::MyLibrary => {
                    let lib = crate::user_library::UserLibrary::new();
                    if let Err(e) = lib.delete(kind, &id) {
                        log::error!("[preset] delete failed: {e}");
                    }
                    ui.browser_popup.close();
                    DispatchResult::handled()
                }
                Source::Project => {
                    let cmd = manifold_editing::commands::preset::DeleteEmbeddedPresetCommand::new(id);
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    ui.browser_popup.close();
                    DispatchResult::structural()
                }
                Source::Factory => unreachable!("returned above"),
            }
        }
        PanelAction::BrowserRevealPresetClicked(mode, type_id) => {
            // My Library only — the menu never offers Reveal for Project
            // (a project-embedded preset has no file to reveal). Doesn't
            // close the popup: a read-only peek shouldn't interrupt browsing.
            let kind = browser_mode_to_kind(*mode);
            let id = manifold_core::PresetTypeId::from_string(type_id.clone());
            crate::user_library::UserLibrary::new().reveal(kind, &id);
            DispatchResult::handled()
        }

        // ── Generator params ───────────────────────────────────────
        PanelAction::GenTypeClicked(_) => DispatchResult::handled(),
        // `ParamToggle`/`ParamFire` (§8.4 P3b): unified effect+generator via
        // the same `resolve_graph_target` + `with_preset_graph_mut` path
        // `ParamChanged`/`ParamCommit` already use, rather than the old
        // `GenParamToggle`/`GenParamFire`'s generator-only `gen_params_mut()`
        // lookup — a click is atomic (no drag), so one command captures the
        // old value and writes the new one in the same arm.
        PanelAction::ParamToggle(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let old_val = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(old_val) = old_val {
                    let new_val = if old_val > 0.5 { 0.0 } else { 1.0 };
                    project.with_preset_graph_mut(&target, |inst| {
                        inst.set_base_param(param_id.as_ref(), new_val);
                    });
                    let cmd = ChangeGraphParamCommand::new(target, param_id.clone(), old_val, new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ParamFire(gpt, param_id) => {
            // Trigger button click: increment the monotonic counter by one.
            // Mirrors ParamToggle's plumbing exactly except the value
            // transform is `+1` instead of `0↔1`.
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let old_val = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(old_val) = old_val {
                    let new_val = old_val + 1.0;
                    project.with_preset_graph_mut(&target, |inst| {
                        inst.set_base_param(param_id.as_ref(), new_val);
                    });
                    let cmd = ChangeGraphParamCommand::new(target, param_id.clone(), old_val, new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }

        // ── "3D Shading" relight (docs/DEPTH_RELIGHT_DESIGN.md D8/P7) ─────
        // The toggle and `height_from` change template topology, so they stay
        // structural. The D3 float knobs are now live uniforms written per
        // frame, so a drag updates the local project + the content thread via
        // `MutateProjectLive` and returns `handled()` — no chain rebuild.
        PanelAction::RelightToggle(gpt) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let old = project.with_preset_graph_mut(&target, |inst| inst.relight).unwrap_or(false);
                let mut cmd = ToggleRelightCommand::new(target, old, !old);
                cmd.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::structural()
        }
        PanelAction::RelightParamSnapshot(gpt, field) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let f = crate::ui_translate::relight_field_to_editing(*field);
                *drag_snapshot =
                    project.with_preset_graph_mut(&target, |inst| f.get(&inst.relight_params));
            }
            DispatchResult::handled()
        }
        PanelAction::RelightParamChanged(gpt, field, val) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let f = crate::ui_translate::relight_field_to_editing(*field);
                let v = *val;
                // Live drag: update the UI-side project immediately so the
                // slider follows the pointer, and mirror to the content thread
                // via `MutateProjectLive`. No `bump_graph_structure_version`
                // — float knobs are per-frame uniforms (D8/P7).
                project.with_preset_graph_mut(&target, |inst| {
                    f.set(&mut inst.relight_params, v);
                });
                let t = target.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        p.with_preset_graph_mut(&t, |inst| {
                            f.set(&mut inst.relight_params, v);
                        });
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::RelightParamCommit(gpt, field) => {
            if let Some(old_val) = drag_snapshot.take()
                && let Some(target) =
                    resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let f = crate::ui_translate::relight_field_to_editing(*field);
                let new_val =
                    project.with_preset_graph_mut(&target, |inst| f.get(&inst.relight_params));
                if let Some(new_val) = new_val
                    && (old_val - new_val).abs() > f32::EPSILON
                {
                    let cmd = SetRelightParamCommand::new(target, f, old_val, new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::RelightHeightFromChanged(gpt, height_from) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let old = project
                    .with_preset_graph_mut(&target, |inst| inst.relight_params.height_from)
                    .unwrap_or_default();
                let new = crate::ui_translate::relight_height_from_to_core(*height_from);
                let mut cmd = SetRelightHeightFromCommand::new(target, old, new);
                cmd.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::structural()
        }

        PanelAction::AddEffect(tab, effect_type) => {
            use manifold_core::effects::PresetInstance;
            // The action carries the chosen preset id directly (registry
            // entries AND project-embedded presets), so no index lookup.
            let effect_type = crate::ui_translate::preset_type_id_to_core(effect_type);
            let defaults = manifold_core::preset_definition_registry::get_defaults(&effect_type);
            let mut effect = PresetInstance::new(effect_type.clone());
            effect.params = manifold_core::params::ParamManifest::from_params(defaults);
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                InspectorTab::Layer | InspectorTab::Group => {
                    if let Some(idx) = layer_idx {
                        let layer_id = project
                            .timeline
                            .layers
                            .get(idx)
                            .map(|l| l.layer_id.clone())
                            .unwrap_or_default();
                        EffectTarget::Layer { layer_id }
                    } else {
                        return DispatchResult::handled();
                    }
                }
                InspectorTab::Clip => {
                    log::debug!("Add effect to clip (clip selection not yet implemented)");
                    return DispatchResult::handled();
                }
            };
            let insert_idx = match &target {
                EffectTarget::Master => project.settings.master_effects.len(),
                EffectTarget::Layer { layer_id } => project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| l.layer_id == *layer_id)
                    .and_then(|l| l.effects.as_ref())
                    .map(|e| e.len())
                    .unwrap_or(0),
            };
            let cmd = manifold_editing::commands::effects::AddEffectCommand::new(
                target, effect, insert_idx,
            );
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }

        PanelAction::PasteEffects => DispatchResult::handled(),

        // Label right-clicks are consumed by try_open_dropdown — shouldn't reach here
        PanelAction::ParamLabelRightClick(..) => {
            DispatchResult::handled()
        }

        // ── Macro mapping ─────────────────────────────────────────
        PanelAction::MapParamToMacro(gpt, param_id, macro_idx) => {
            use manifold_core::{MacroCurve, MacroMapping};
            let macro_idx = *macro_idx;
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
                && let Some(mapping_target) = macro_mapping_target(&target, param_id)
            {
                // Graph-authority-first range so a generator's (or graph-backed
                // effect's) true slider range isn't squashed to the registry's.
                let (min, max) = project
                    .with_preset_graph_mut(&target, |inst| resolve_param_range(inst, param_id.as_ref()))
                    .unwrap_or((0.0, 1.0));
                let mapping = MacroMapping {
                    target: mapping_target,
                    range_min: min,
                    range_max: max,
                    curve: MacroCurve::Linear,
                    legacy_param_index: None,
                    legacy_effect_addr: None,
                };
                project.settings.macro_bank.slots[macro_idx]
                    .mappings
                    .push(mapping.clone());
                let mi = macro_idx;
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        p.settings.macro_bank.slots[mi].mappings.push(mapping);
                    })),
                );
            }
            DispatchResult::handled()
        }
        // Label right-click consumed by try_open_dropdown — shouldn't reach here
        PanelAction::MacroLabelRightClick(_) => DispatchResult::handled(),

        PanelAction::UnmapMacro(macro_idx, mapping_idx) => {
            let macro_idx = *macro_idx;
            let mapping_idx = *mapping_idx;
            if macro_idx < manifold_core::MACRO_COUNT {
                let slot = &mut project.settings.macro_bank.slots[macro_idx];
                if mapping_idx < slot.mappings.len() {
                    slot.mappings.remove(mapping_idx);
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::MutateProject(Box::new(move |p| {
                            let slot = &mut p.settings.macro_bank.slots[macro_idx];
                            if mapping_idx < slot.mappings.len() {
                                slot.mappings.remove(mapping_idx);
                            }
                        })),
                    );
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClearMacroMappings(macro_idx) => {
            let macro_idx = *macro_idx;
            if macro_idx < manifold_core::MACRO_COUNT {
                project.settings.macro_bank.slots[macro_idx]
                    .mappings
                    .clear();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        p.settings.macro_bank.slots[macro_idx].mappings.clear();
                    })),
                );
            }
            DispatchResult::handled()
        }

        // ── Ableton mapping ────────────────────────────────────────
        // Map + unmap run ONE path: resolve the unified `GraphTarget`, derive
        // the `AbletonMappingTarget` via the shared `ableton_mapping_target`
        // helper (effect by stable EffectId within master/layer; generator by
        // layer; clip tab → None, no clip-scoped Ableton mappings), then send
        // the content command. Mirrors `UnmapParamAbleton` below exactly — the
        // only difference is AbletonMapParam (with address) vs AbletonUnmapParam.
        PanelAction::MapParamToAbleton(gpt, param_id, address) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
            {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::AbletonMapParam {
                        target: mapping_target,
                        address: crate::ui_translate::ableton_macro_address_to_core(address),
                    },
                );
            }
            DispatchResult::handled()
        }
        PanelAction::UnmapParamAbleton(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
            {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::AbletonUnmapParam {
                        target: mapping_target,
                    },
                );
            }
            DispatchResult::handled()
        }

        PanelAction::MapMacroToAbleton(slot_idx, address) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            let target = AbletonMappingTarget::MacroSlot {
                slot_index: *slot_idx,
            };
            ContentCommand::send(
                content_tx,
                ContentCommand::AbletonMapParam {
                    target,
                    address: crate::ui_translate::ableton_macro_address_to_core(address),
                },
            );
            DispatchResult::handled()
        }
        PanelAction::UnmapMacroAbleton(slot_idx) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            let target = AbletonMappingTarget::MacroSlot {
                slot_index: *slot_idx,
            };
            ContentCommand::send(content_tx, ContentCommand::AbletonUnmapParam { target });
            DispatchResult::handled()
        }
        // Picker open is consumed by try_open_dropdown — never reaches dispatch.
        PanelAction::OpenAbletonPickerForMacro(_) => DispatchResult::handled(),

        // Driver / Ableton / audio trim handles are unified into the
        // `Trim{Changed,Snapshot,Commit}(TrimKind, …)` arms above.

        PanelAction::AbletonMacroTrimChanged(slot_idx, min, max) => {
            let slot_idx = *slot_idx;
            let min = *min;
            let max = *max;
            if let Some(slot) = project.settings.macro_bank.slots.get_mut(slot_idx)
                && let Some(m) = &mut slot.ableton_mapping
            {
                m.range_min = min;
                m.range_max = max;
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(slot) = p.settings.macro_bank.slots.get_mut(slot_idx)
                        && let Some(m) = &mut slot.ableton_mapping
                    {
                        m.range_min = min;
                        m.range_max = max;
                    }
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::AbletonMacroTrimSnapshot(slot_idx) => {
            if let Some(range) = project
                .settings
                .macro_bank
                .slots
                .get(*slot_idx)
                .and_then(|s| s.ableton_mapping.as_ref())
                .map(|m| (m.range_min, m.range_max))
            {
                *trim_snapshot = Some(range);
            }
            DispatchResult::handled()
        }
        PanelAction::AbletonMacroTrimCommit(slot_idx) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            if let Some((old_min, old_max)) = trim_snapshot.take()
                && let Some((new_min, new_max)) = project
                    .settings
                    .macro_bank
                    .slots
                    .get(*slot_idx)
                    .and_then(|s| s.ableton_mapping.as_ref())
                    .map(|m| (m.range_min, m.range_max))
                && ((old_min - new_min).abs() > f32::EPSILON
                    || (old_max - new_max).abs() > f32::EPSILON)
            {
                let cmd = ChangeAbletonTrimCommand::new(
                    AbletonMappingTarget::MacroSlot { slot_index: *slot_idx },
                    old_min,
                    old_max,
                    new_min,
                    new_max,
                );
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::handled()
        }

        PanelAction::AbletonInvertToggle(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
            {
                if let Some(ms) = project
                    .ableton_param_mappings_mut(&mapping_target)
                    .and_then(|opt| opt.as_mut())
                    && let Some(m) = ms.iter_mut().find(|m| m.param_id == *param_id)
                {
                    m.inverted = !m.inverted;
                }
                let mt = mapping_target.clone();
                let pid = param_id.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        if let Some(ms) =
                            p.ableton_param_mappings_mut(&mt).and_then(|opt| opt.as_mut())
                            && let Some(m) = ms.iter_mut().find(|m| m.param_id == pid)
                        {
                            m.inverted = !m.inverted;
                        }
                    })),
                );
            }
            DispatchResult::structural()
        }

        PanelAction::AbletonMacroInvertToggle(slot_idx) => {
            let slot_idx = *slot_idx;
            if let Some(slot) = project.settings.macro_bank.slots.get_mut(slot_idx)
                && let Some(m) = &mut slot.ableton_mapping
            {
                m.inverted = !m.inverted;
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(slot) = p.settings.macro_bank.slots.get_mut(slot_idx)
                        && let Some(m) = &mut slot.ableton_mapping
                    {
                        m.inverted = !m.inverted;
                    }
                })),
            );
            DispatchResult::structural()
        }

        _ => DispatchResult::unhandled(),
    }
}

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
    use super::*;
    use crate::content_command::ContentCommand;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::SerializedParamValue;
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

    fn density_node_and_value(def: &manifold_core::effect_graph_def::EffectGraphDef) -> (u32, f32) {
        let vm = SceneVm::from_def(def).expect("SceneStarter resolves as a scene");
        let AtmosphereVm::Wired(a) = vm.atmosphere else {
            panic!("SceneStarter's atmosphere must be Wired");
        };
        (a.density_addr.node_doc_id, a.density_value)
    }

    /// Configure `ui.scene_setup_panel` with a REAL Live VM for the fixture
    /// layer — exercises `ScenePanel::build_docked`'s actual id-map
    /// construction (D2), not a hand-poked shortcut.
    fn open_scene_panel_on_fog_density(ui: &mut UIRoot, project: &Project, layer_id: &LayerId) -> u32 {
        use manifold_ui::panels::scene_setup_panel::{
            AtmosphereRowVm, CameraRowVm, EnvironmentRowVm, ModulatedRow, RowAddr, RowModulation,
            RowValue, SceneSetupState, SceneSetupVm,
        };
        let def = fog_density_addr(project, layer_id);
        let (node_doc_id, value) = density_node_and_value(&def);
        ui.scene_setup_panel.open();
        ui.scene_setup_panel.configure(SceneSetupState::Live(Box::new(SceneSetupVm {
            layer_id: layer_id.clone(),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 0,
            light_count: 0,
            shadow_caster_count: 0,
            scene_root_node_id: 0,
            environment: EnvironmentRowVm::None,
            atmosphere: AtmosphereRowVm::Wired {
                density: ModulatedRow {
                    value: RowValue {
                        addr: RowAddr::root(node_doc_id, "fog_density"),
                        value,
                        min: 0.0,
                        max: 1.0,
                        driven: false,
                        exposed: false,
                    },
                    modulation: Box::new(RowModulation::default()),
                },
                height_falloff: ModulatedRow {
                    value: RowValue {
                        addr: RowAddr::root(node_doc_id, "height_falloff"),
                        value: 0.3,
                        min: 0.0,
                        max: 2.0,
                        driven: false,
                        exposed: false,
                    },
                    modulation: Box::new(RowModulation::default()),
                },
            },
            audio_send_labels: Vec::new(),
            audio_send_ids: Vec::new(),
            objects: Vec::new(),
            lights: Vec::new(),
            camera: CameraRowVm::None,
        })));
        let mut tree = manifold_ui::tree::UITree::new();
        let dock = manifold_ui::node::Rect::new(0.0, 0.0, 400.0, 800.0);
        let region = tree.begin_region(dock, manifold_ui::ZTier::Base, "scene_setup_test", manifold_ui::node::UIFlags::empty());
        let start = tree.count();
        ui.scene_setup_panel.build_docked(&mut tree, dock);
        tree.end_region(region, start);
        node_doc_id
    }

    #[allow(clippy::type_complexity)]
    struct Harness {
        content_tx: crossbeam_channel::Sender<ContentCommand>,
        content_rx: crossbeam_channel::Receiver<ContentCommand>,
        content_state: crate::content_state::ContentState,
        ui: UIRoot,
        selection: SelectionState,
        active_layer: Option<LayerId>,
        drag_snapshot: Option<f32>,
        trim_snapshot: Option<(f32, f32)>,
        target_snapshot: Option<f32>,
        decay_snapshot: Option<f32>,
        audio_shape_snapshot: Option<manifold_core::audio_mod::AudioModShape>,
        audio_action_snapshot: Option<manifold_core::audio_mod::TriggerAction>,
        audio_crossover_snapshot: Option<(f32, f32)>,
        audio_send_gain_drag_snapshot: Option<f32>,
        active_inspector_drag: Option<crate::app::ActiveInspectorDrag>,
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
                drag_snapshot: None,
                trim_snapshot: None,
                target_snapshot: None,
                decay_snapshot: None,
                audio_shape_snapshot: None,
                audio_action_snapshot: None,
                audio_crossover_snapshot: None,
                audio_send_gain_drag_snapshot: None,
                active_inspector_drag: None,
            }
        }

        fn dispatch(&mut self, action: &PanelAction, project: &mut Project) -> DispatchResult {
            dispatch_inspector(
                action,
                project,
                &self.content_tx,
                &self.content_state,
                &mut self.ui,
                &mut self.selection,
                &mut self.active_layer,
                &mut self.drag_snapshot,
                &mut self.trim_snapshot,
                &mut self.target_snapshot,
                &mut self.decay_snapshot,
                &mut self.audio_shape_snapshot,
                &mut self.audio_action_snapshot,
                &mut self.audio_crossover_snapshot,
                &mut self.audio_send_gain_drag_snapshot,
                &mut self.active_inspector_drag,
                None,
            )
        }

        fn drain(&self) -> Vec<ContentCommand> {
            self.content_rx.try_iter().collect()
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
        h.dispatch(&PanelAction::MacroSnapshot(0), &mut project);
        h.dispatch(&PanelAction::MacroChanged(0, 0.8), &mut project);
        h.drain();

        // What the UI frame drain now does every tick (app_render.rs ~line
        // 868): apply the snapshot, then restore only the guarded drag kinds.
        stale.apply(&mut project);
        if let Some(ref drag) = h.active_inspector_drag {
            drag.apply(&mut project);
        }

        h.dispatch(&PanelAction::MacroCommit(0), &mut project);
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
        use crate::app::ActiveInspectorDrag;
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

        // app_render mid-drag: local_project := stale, then restore the drag.
        let drag = ActiveInspectorDrag::Trim {
            kind: manifold_ui::panels::TrimKind::Driver,
            target: target.clone(),
            ableton_target: None,
            param_id: pid.clone(),
            min: 0.3,
            max: 0.9,
        };
        let mut local = stale;
        drag.apply(&mut local);

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

    /// BUG-249 gate (expose-then-arm): a `DriverToggle` on a scene row's
    /// synthesized id must (1) materialize a REAL exposed instance param
    /// via the same `ToggleNodeParamExposeCommand` path the mod button
    /// dispatches, and (2) store the driver keyed by that real binding id —
    /// present in `inst.params`, which is the ONLY namespace the runtime
    /// (`modulation.rs`) resolves — never by the synth id, which the
    /// runtime silently drops. Also proves re-toggle reuses the binding
    /// (no duplicate exposure) and the panel read-back translation
    /// (`scene_row_modulation`) reports the armed driver for the row.
    #[test]
    fn scene_row_driver_toggle_arms_a_real_exposed_param() {
        let (mut project, layer_id) = scene_layer_project();
        let mut h = Harness::new(Some(layer_id.clone()));
        let node_doc_id = open_scene_panel_on_fog_density(&mut h.ui, &project, &layer_id);
        // The pid a real click carries: the panel's curated row key
        // ("density"), NOT the graph key ("fog_density") — the id map's
        // VALUE carries the graph address, its KEY uses the panel key.
        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(node_doc_id, "density");

        h.dispatch(
            &PanelAction::DriverToggle(manifold_ui::GraphParamTarget::Generator, pid.clone()),
            &mut project,
        );

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst = layer.gen_params().expect("arming must init gen_params");
        let real_id = inst
            .binding_id_for_node_param(node_doc_id, "fog_density")
            .expect("first arm must materialize an exposed param binding");
        assert_ne!(real_id, pid.as_ref(), "the binding id must not be the synth id");
        assert!(
            inst.params.contains(real_id.as_str()),
            "the exposed param must exist in inst.params — the namespace modulation.rs resolves"
        );
        let drivers = inst.drivers.as_ref().expect("driver must be stored");
        let d = drivers
            .iter()
            .find(|d| d.param_id.as_ref() == real_id)
            .expect("driver must be keyed by the REAL binding id");
        assert!(d.enabled);
        assert!(
            !drivers.iter().any(|d| d.param_id == pid),
            "no driver may be keyed by the runtime-unresolvable synth id"
        );
        // base_value captured off the real param, not a garbage read of a
        // param that doesn't exist (SceneStarter ships fog_density 0.04).
        assert!((d.base_value - 0.04).abs() < 1e-6, "base_value = {}", d.base_value);

        // Read-back half: the panel's per-row modulation lookup translates
        // the row address to the real binding and sees the armed driver.
        let row = crate::ui_bridge::state_sync::scene_row_modulation(
            Some(inst),
            None,
            node_doc_id,
            "fog_density",
            &[],
        );
        assert!(row.driver_active, "scene row read-back must report the armed driver");

        // Both mutations reached the content thread as undo-tracked
        // commands: the exposure + the driver add.
        let executes = h
            .drain()
            .iter()
            .filter(|c| matches!(c, ContentCommand::Execute(_)))
            .count();
        assert_eq!(executes, 2, "expose + arm = two Execute commands");

        // Second toggle: translate-only — reuses the binding (no duplicate
        // exposure) and flips the SAME driver off.
        h.dispatch(
            &PanelAction::DriverToggle(manifold_ui::GraphParamTarget::Generator, pid.clone()),
            &mut project,
        );
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst = layer.gen_params().unwrap();
        assert_eq!(
            inst.binding_id_for_node_param(node_doc_id, "fog_density").as_deref(),
            Some(real_id.as_str()),
            "re-toggle must not mint a second binding"
        );
        let drivers = inst.drivers.as_ref().unwrap();
        assert_eq!(drivers.len(), 1);
        assert!(!drivers[0].enabled, "second toggle disarms the same driver");
    }

    /// Same race, scene-row flavor: a full project snapshot accepted
    /// mid-drag (data_version bump from any concurrent Execute — MIDI
    /// phantom commit, another gesture landing) replaces `local_project`
    /// wholesale; the guard's restore must reach the row's REAL write
    /// address (`SceneParam` variant), because the old `Param` restore was
    /// a silent no-op for synthesized scene ids.
    #[test]
    fn scene_row_drag_survives_a_full_snapshot_replacement() {
        let (mut project, layer_id) = scene_layer_project();
        let mut h = Harness::new(Some(layer_id.clone()));
        let node_doc_id = open_scene_panel_on_fog_density(&mut h.ui, &project, &layer_id);
        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(node_doc_id, "density");
        let target = manifold_ui::GraphParamTarget::Generator;
        let before = density_node_and_value(&fog_density_addr(&project, &layer_id)).1;

        // The content thread's stale full snapshot, captured pre-drag.
        let stale = project.clone();

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), before + 0.3), &mut project);
        h.drain();

        // Simulate app_render's snapshot acceptance mid-drag: replace the
        // project, then restore the guarded drag (app_render.rs ~line 817).
        project = stale;
        if let Some(ref drag) = h.active_inspector_drag {
            drag.apply(&mut project);
        }

        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let cmds = h.drain();
        let mut execs: Vec<_> = cmds
            .into_iter()
            .filter_map(|c| match c {
                ContentCommand::Execute(cmd) => Some(cmd),
                _ => None,
            })
            .collect();
        assert_eq!(execs.len(), 1, "the commit must survive the snapshot stomp as ONE undo unit");
        let cmd = &mut execs[0];
        cmd.execute(&mut project);
        let after_execute = density_node_and_value(&fog_density_addr(&project, &layer_id)).1;
        assert!((after_execute - (before + 0.3)).abs() < 1e-4, "commit lands the dragged value");
        cmd.undo(&mut project);
        let after_undo = density_node_and_value(&fog_density_addr(&project, &layer_id)).1;
        assert!((after_undo - before).abs() < 1e-4, "undo restores the pre-drag value");
    }

    /// Gate 2 (undo-granularity): `ParamSnapshot` → 3× `ParamChanged` →
    /// `ParamCommit` yields exactly ONE `ContentCommand::Execute` (the
    /// undo-tracked commit) and zero more, plus 3 `MutateProjectLive`s (the
    /// live scrub ticks) — never one `Execute` per tick.
    #[test]
    fn fog_density_drag_session_yields_exactly_one_undo_entry() {
        let (mut project, layer_id) = scene_layer_project();
        let mut h = Harness::new(Some(layer_id.clone()));
        let layer_idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
        h.active_layer = Some(layer_id.clone());
        // `resolve_active_layer_index` (behind `resolve_graph_target`)
        // walks `active_layer` as a `LayerId`, but `dispatch_inspector`'s
        // own signature takes an index-derived `active_layer: &mut Option<LayerId>`
        // — already set above; `layer_idx` only proves the fixture resolves.
        let _ = layer_idx;

        let node_doc_id = open_scene_panel_on_fog_density(&mut h.ui, &project, &layer_id);
        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(node_doc_id, "density");
        let target = manifold_ui::GraphParamTarget::Generator;

        let before = density_node_and_value(&fog_density_addr(&project, &layer_id)).1;

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        assert!(h.drain().is_empty(), "Snapshot sends no ContentCommand");

        for v in [before + 0.1, before + 0.2, before + 0.3] {
            h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), v), &mut project);
        }
        let mid_commands = h.drain();
        assert_eq!(mid_commands.len(), 3, "3 live ticks");
        assert!(
            mid_commands.iter().all(|c| matches!(c, ContentCommand::MutateProjectLive(_))),
            "every motion tick is a live (non-undoable) write, never Execute"
        );

        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let commit_commands = h.drain();
        assert_eq!(commit_commands.len(), 1, "exactly ONE command on release — the undo unit");
        let ContentCommand::Execute(mut cmd) = commit_commands.into_iter().next().unwrap() else {
            panic!("the commit command must be undo-tracked (Execute), not MutateProjectLive");
        };

        // The UI-thread `project` already reflects the live ticks (D4's
        // cadence writes locally too) — the commit command's execute() is a
        // same-value no-op there; what matters is its `undo()` restores the
        // TRUE pre-drag value (BUG found+fixed this session: a naive
        // self-captured `SetGraphNodeParamCommand` would instead capture
        // the POST-drag value and undo to a no-op — see `with_previous`'s
        // doc comment).
        cmd.execute(&mut project);
        let after_execute = density_node_and_value(&fog_density_addr(&project, &layer_id)).1;
        assert!((after_execute - (before + 0.3)).abs() < 1e-4, "commit lands the final dragged value");

        cmd.undo(&mut project);
        let after_undo = density_node_and_value(&fog_density_addr(&project, &layer_id)).1;
        assert!(
            (after_undo - before).abs() < 1e-4,
            "undo must restore the PRE-DRAG value ({before}), got {after_undo}"
        );
    }

    /// Gate 4 (imported-def value test): the fog-density write lands in the
    /// layer's OWN instance def (`Layer::generator_graph()`) — the same
    /// per-layer override an imported/migrated scene graph lives in, not
    /// just the pristine bundled catalog default. Mirrors C7's SceneStarter
    /// precedent (`project.rs::scene_layer_project`) applied to a value
    /// write instead of a structural add.
    #[test]
    fn fog_density_commit_writes_the_layer_instance_def() {
        let (mut project, layer_id) = scene_layer_project();
        // Before any edit, the layer has NO instance override yet — reads
        // fall back to the catalog default (SetGraphNodeParamCommand lifts
        // one on first write, same as every other graph command family).
        assert!(
            project.timeline.find_layer_by_id(&layer_id).unwrap().1.generator_graph().is_none(),
            "a fresh layer has no per-instance override yet"
        );

        let mut h = Harness::new(Some(layer_id.clone()));
        let node_doc_id = open_scene_panel_on_fog_density(&mut h.ui, &project, &layer_id);
        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(node_doc_id, "density");
        let target = manifold_ui::GraphParamTarget::Generator;

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), 0.66), &mut project);
        h.drain();
        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let ContentCommand::Execute(mut cmd) = h.drain().into_iter().next().unwrap() else {
            panic!("expected Execute");
        };
        cmd.execute(&mut project);

        // Lifted an instance override (ParamChanged's local write already
        // did this via `SetGraphNodeParamCommand::execute`'s
        // `get_or_insert_with(catalog_default)`).
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst_def = layer.generator_graph().expect("the write must land in the layer's OWN instance def");
        let node = inst_def.nodes.iter().find(|n| n.id == node_doc_id).unwrap();
        match node.params.get("fog_density") {
            Some(SerializedParamValue::Float { value }) => {
                assert!((value - 0.66).abs() < 1e-4, "instance def must carry the committed value, got {value}");
            }
            other => panic!("expected a Float fog_density in the instance def, got {other:?}"),
        }
    }

    /// Diagnostic for a discrepancy found in the manual `--script` flow
    /// (`scene-setup-fog-density-card-row.json`): after `SceneSetupAddFog`
    /// (dispatched via `dispatch_project`, a DIFFERENT sub-dispatcher than
    /// `dispatch_inspector`) creates the fog node, then a Snapshot→3×Changed→
    /// Commit session via `dispatch_inspector` on THAT node — does the value
    /// actually persist? Isolates whether the flow script's "still shows
    /// 0.00" symptom is a real write bug or a headless-harness-only
    /// display/rebuild-cadence artifact.
    #[test]
    fn fog_density_write_persists_after_add_fog_then_drag_session() {
        use manifold_core::effect_graph_def::EffectGraphDef;
        use manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;

        let mut project = Project::default();
        let idx = project.timeline.add_layer(
            "Scene",
            LayerType::Generator,
            PresetTypeId::from_string("SceneStarter".to_string()),
        );
        let layer_id = project.timeline.layers[idx].layer_id.clone();

        // Strip SceneStarter's default fog so this starts in the SAME
        // "Atmosphere::None" state gltfscene's post-import layer is in —
        // an explicit instance override with the atmosphere node + its
        // wire removed.
        let def: EffectGraphDef = manifold_renderer::node_graph::bundled_preset_def(
            &project.timeline.layers[idx].generator_type().clone(),
        )
        .cloned()
        .expect("SceneStarter is a bundled preset");
        let render_scene_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
            .expect("SceneStarter has a render_scene node")
            .id;
        let atmosphere_node_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.atmosphere")
            .expect("SceneStarter has a node.atmosphere node")
            .id;
        let mut stripped = def.clone();
        stripped.nodes.retain(|n| n.id != atmosphere_node_id);
        stripped.wires.retain(|w| w.from_node != atmosphere_node_id && w.to_node != atmosphere_node_id);
        project.timeline.layers[idx].gen_params_or_init().graph = Some(stripped);

        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut active_layer = Some(layer_id.clone());
        let mut user_prefs = crate::user_prefs::UserPrefs::load();

        // 1) "+ Add Fog" — the SAME `dispatch_project` sub-dispatcher the
        // panel's button click reaches (a DIFFERENT function than
        // `dispatch_inspector`, which the drag session below uses).
        let add_fog = PanelAction::SceneSetupAddFog(layer_id.clone(), render_scene_id);
        let result = super::super::project::dispatch_project(
            &add_fog, &mut project, &content_tx, &content_state, &mut ui, &mut selection,
            &mut active_layer, &mut user_prefs,
        );
        assert!(result.structural_change, "AddFog is a structural graph edit");
        content_rx.try_iter().for_each(drop);

        // 2) Open the scene panel on the FRESH state (id map now sees the
        // newly added fog node) — mirrors the flow script's own re-open.
        let node_doc_id = open_scene_panel_on_fog_density(&mut ui, &project, &layer_id);
        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(node_doc_id, "density");
        let target = manifold_ui::GraphParamTarget::Generator;

        let mut h = Harness::new(Some(layer_id.clone()));
        h.ui = ui;
        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        for v in [0.5, 0.95, 1.0] {
            h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), v), &mut project);
        }
        h.drain();
        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let ContentCommand::Execute(mut cmd) = h.drain().into_iter().next().unwrap() else {
            panic!("expected Execute — the write must be undo-tracked even after Add Fog");
        };
        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst_def = layer.generator_graph().expect("instance override must exist post-AddFog");
        let node = inst_def.nodes.iter().find(|n| n.id == node_doc_id).unwrap();
        match node.params.get("fog_density") {
            Some(SerializedParamValue::Float { value }) => {
                assert!((value - 1.0).abs() < 1e-3, "post-AddFog commit must land 1.0, got {value}");
            }
            other => panic!("expected a Float fog_density, got {other:?}"),
        }
    }

    // ── C-P1b: Object family ────────────────────────────────────────
    // SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1b gates, mirroring C-P1a's
    // fog-density gates above for the converted Object family. Uses the
    // REAL `AddSceneObjectCommand` (via `dispatch_project`'s
    // `SceneSetupAddObject` arm, the panel's own "+ Object" button path) —
    // this command wraps the new object in a `GROUP_TYPE_ID` node and nests
    // its `node.transform_3d`/material inside that group
    // (`crates/manifold-editing/src/commands/graph.rs`'s
    // `AddSceneObjectCommand::execute`), so Position/Rotation/Scale are
    // ALREADY the D12 group-`scope_path` case for every object this command
    // creates — the same shape a gltfscene import produces. This is the
    // family C-P1a's own `read_scene_node_param` explicitly left
    // unsupported (root-scope only); C-P1b extends it via
    // `descend_to_scope` (see that fn's doc comment) — these tests are what
    // would have caught the gap silently no-op'ing `ParamCommit`.

    use manifold_renderer::node_graph::scene_vm::{MaterialVm, SceneObjectVm};

    /// Add one object to a fresh SceneStarter layer via the REAL
    /// `SceneSetupAddObject` dispatch path, and resolve its transform's
    /// write address off the SAME `SceneVm::from_def` production code walks.
    /// SceneStarter ships with 2 objects already wired — `next_index` must
    /// be the REAL current count (mirrors `project.rs`'s own
    /// `scene_setup_add_object_dispatches_add_scene_object_command` fixture,
    /// `before as u32`), or the new object's `object_k` wire collides with
    /// an existing one. Returns `(project, layer_id, object_node_id,
    /// transform_node_doc_id, group_node_id)` for the NEWLY ADDED object
    /// (the last one in `vm.objects`) — `object_node_id` is needed to
    /// explicitly select it (D7's default selection resolves the FIRST
    /// Known object, which for SceneStarter is one of its own 2 built-in
    /// objects, not the one this fixture adds).
    fn scene_layer_with_one_object() -> (Project, LayerId, u32, u32, u32) {
        let (mut project, layer_id) = scene_layer_project();
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let def = manifold_renderer::node_graph::bundled_preset_def(&layer.generator_type().clone())
            .cloned()
            .expect("SceneStarter is a bundled preset");
        let render_scene_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID)
            .expect("SceneStarter has a render_scene node")
            .id;
        let next_index = match def.nodes.iter().find(|n| n.id == render_scene_id).and_then(|n| n.params.get("objects")) {
            Some(SerializedParamValue::Float { value }) => *value as u32,
            _ => 0,
        };

        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut active_layer = Some(layer_id.clone());
        let mut user_prefs = crate::user_prefs::UserPrefs::load();
        let add_object = PanelAction::SceneSetupAddObject(layer_id.clone(), render_scene_id, next_index);
        let result = super::super::project::dispatch_project(
            &add_object, &mut project, &content_tx, &content_state, &mut ui, &mut selection,
            &mut active_layer, &mut user_prefs,
        );
        assert!(result.structural_change, "AddObject is a structural graph edit");
        content_rx.try_iter().for_each(drop);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst_def = layer.generator_graph().expect("AddObject must lift an instance override");
        let vm = SceneVm::from_def(inst_def).expect("the layer still resolves as a scene");
        let SceneObjectVm::Known(obj) = vm.objects.last().expect("the new object was added") else {
            panic!("the added object must resolve Known");
        };
        let group_node_id = obj.group_node_id.expect("AddSceneObjectCommand wraps the object in a group (D12)");
        let transform = obj.transform.as_ref().expect("the added object has a transform_3d");
        assert_eq!(
            transform.pos_addr.0.scope_path,
            vec![group_node_id],
            "AddSceneObjectCommand nests transform_3d INSIDE the group — pos_x is scope_path=[group_node_id], not root"
        );
        (project, layer_id, obj.object_node_id, transform.node_doc_id, group_node_id)
    }

    /// Build a real `Live` scene VM for `layer_id` via the actual
    /// `sync_inspector_data` production path (not a hand-rolled VM),
    /// explicitly selects `object_node_id` (D7's default selection resolves
    /// the FIRST Known object, not necessarily the one under test), then
    /// builds the panel's real tree so `ui.scene_setup_panel`'s
    /// `object_card` id map is populated by `build_object_card_row`.
    fn open_scene_panel_via_sync(ui: &mut UIRoot, project: &Project, layer_id: &LayerId, object_node_id: u32) {
        let layer_idx = project.timeline.find_layer_index_by_id(layer_id).unwrap();
        ui.scene_setup_panel.open();
        ui.scene_setup_panel.set_selection(
            layer_id.clone(),
            manifold_ui::panels::scene_setup_panel::SceneSelection::Object(object_node_id),
        );
        super::super::state_sync::sync_inspector_data(ui, project, Some(layer_idx), &manifold_ui::UIState::new(), &[]);
        let mut tree = manifold_ui::tree::UITree::new();
        let dock = manifold_ui::node::Rect::new(0.0, 0.0, 400.0, 800.0);
        let region = tree.begin_region(dock, manifold_ui::ZTier::Base, "scene_setup_test", manifold_ui::node::UIFlags::empty());
        let start = tree.count();
        ui.scene_setup_panel.build_docked(&mut tree, dock);
        tree.end_region(region, start);
    }

    /// Gate 2 (undo-granularity), Object family: a Position-X drag session
    /// through the group-scoped card row yields exactly ONE undo entry that
    /// restores the pre-drag value — same shape as C-P1a's fog-density gate,
    /// proven on a row whose `scope_path` is non-empty (the case C-P1a never
    /// exercised).
    #[test]
    fn object_position_x_drag_session_yields_exactly_one_undo_entry() {
        let (mut project, layer_id, object_node_id, transform_node_doc_id, _group_node_id) =
            scene_layer_with_one_object();
        let mut h = Harness::new(Some(layer_id.clone()));
        open_scene_panel_via_sync(&mut h.ui, &project, &layer_id, object_node_id);

        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(transform_node_doc_id, "pos_x");
        let target = manifold_ui::GraphParamTarget::Generator;
        let before = 0.0_f32; // AddSceneObjectCommand's fresh transform_3d has no pos_x override.

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        assert!(h.drain().is_empty(), "Snapshot sends no ContentCommand");

        for v in [1.0, 2.0, 3.0] {
            h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), v), &mut project);
        }
        let mid_commands = h.drain();
        assert_eq!(mid_commands.len(), 3, "3 live ticks");
        assert!(
            mid_commands.iter().all(|c| matches!(c, ContentCommand::MutateProjectLive(_))),
            "every motion tick is a live (non-undoable) write, never Execute"
        );

        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let commit_commands = h.drain();
        assert_eq!(commit_commands.len(), 1, "exactly ONE command on release — the undo unit");
        let ContentCommand::Execute(mut cmd) = commit_commands.into_iter().next().unwrap() else {
            panic!("the commit command must be undo-tracked (Execute), not MutateProjectLive");
        };

        cmd.execute(&mut project);
        let read_pos_x = |p: &Project| {
            let (_, layer) = p.timeline.find_layer_by_id(&layer_id).unwrap();
            let vm = SceneVm::from_def(layer.generator_graph().unwrap()).unwrap();
            let SceneObjectVm::Known(obj) = vm.objects.last().unwrap() else { panic!("must be Known") };
            obj.transform.as_ref().unwrap().pos_value.0
        };
        assert!((read_pos_x(&project) - 3.0).abs() < 1e-4, "commit lands the final dragged value");

        cmd.undo(&mut project);
        assert!(
            (read_pos_x(&project) - before).abs() < 1e-4,
            "undo must restore the PRE-DRAG value ({before}), got {}",
            read_pos_x(&project)
        );
    }

    /// Gate 4 (imported-def value test), Object family: a Position-X commit
    /// lands in the layer's OWN instance def, AT THE CORRECT GROUP SCOPE —
    /// this is the family where D12 scoping actually bites (C-P1a's own
    /// `read_scene_node_param` was root-scope-only; a naive port would have
    /// silently no-op'd every Object-family `ParamCommit`, per that fn's own
    /// doc comment).
    #[test]
    fn object_position_x_commit_writes_the_layer_instance_def_at_group_scope() {
        let (mut project, layer_id, object_node_id, transform_node_doc_id, group_node_id) =
            scene_layer_with_one_object();
        let mut h = Harness::new(Some(layer_id.clone()));
        open_scene_panel_via_sync(&mut h.ui, &project, &layer_id, object_node_id);

        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(transform_node_doc_id, "pos_x");
        let target = manifold_ui::GraphParamTarget::Generator;

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), 5.5), &mut project);
        h.drain();
        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let ContentCommand::Execute(mut cmd) = h.drain().into_iter().next().unwrap() else {
            panic!("expected Execute");
        };
        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst_def = layer.generator_graph().expect("the write must land in the layer's OWN instance def");
        let group = inst_def.nodes.iter().find(|n| n.id == group_node_id).expect("the group node must still exist");
        let body = group.group.as_ref().expect("the group must carry a body");
        let node = body.nodes.iter().find(|n| n.id == transform_node_doc_id).expect(
            "the transform node must be found INSIDE the group body — a root-only lookup would miss it entirely",
        );
        match node.params.get("pos_x") {
            Some(SerializedParamValue::Float { value }) => {
                assert!((value - 5.5).abs() < 1e-4, "instance def must carry the committed value, got {value}");
            }
            other => panic!("expected a Float pos_x in the group-scoped instance def, got {other:?}"),
        }
    }

    /// Gate 4's Roughness half: `AddSceneObjectCommand` spawns a
    /// `node.phong_material` (no metallic/roughness — D4's "the atom's own
    /// params otherwise"), so this test converts the added object's
    /// material node to `node.pbr_material` directly on the instance def
    /// (the same shape a PBR-material gltf import produces) to exercise the
    /// Roughness card row specifically, at the SAME group scope Position-X
    /// proved above.
    #[test]
    fn object_roughness_commit_writes_the_layer_instance_def_at_group_scope() {
        let (mut project, layer_id, object_node_id, _transform_node_doc_id, group_node_id) =
            scene_layer_with_one_object();
        let mat_node_doc_id = {
            let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
            let inst_def = layer.generator_graph().unwrap();
            let group = inst_def.nodes.iter().find(|n| n.id == group_node_id).unwrap();
            let body = group.group.as_ref().unwrap();
            body.nodes
                .iter()
                .find(|n| n.type_id == "node.phong_material")
                .expect("AddSceneObjectCommand spawns a phong material")
                .id
        };
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&layer_id).unwrap();
            let inst_def = layer.gen_params_or_init().graph.as_mut().unwrap();
            let group = inst_def.nodes.iter_mut().find(|n| n.id == group_node_id).unwrap();
            let body = group.group.as_mut().unwrap();
            let mat_node = body.nodes.iter_mut().find(|n| n.id == mat_node_doc_id).unwrap();
            mat_node.type_id = "node.pbr_material".to_string();
            mat_node.params.insert("metallic".to_string(), SerializedParamValue::Float { value: 0.0 });
            mat_node.params.insert("roughness".to_string(), SerializedParamValue::Float { value: 0.5 });
        }
        // Sanity: the def now resolves Roughness as a real Pbr material row
        // before driving any dispatch.
        {
            let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
            let vm = SceneVm::from_def(layer.generator_graph().unwrap()).unwrap();
            let SceneObjectVm::Known(obj) = vm.objects.last().unwrap() else { panic!("must be Known") };
            let MaterialVm::Known(m) = &obj.material else { panic!("must resolve Known material") };
            assert!(m.metallic_roughness.is_some(), "converted material must resolve metallic_roughness");
        }

        let mut h = Harness::new(Some(layer_id.clone()));
        open_scene_panel_via_sync(&mut h.ui, &project, &layer_id, object_node_id);

        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(mat_node_doc_id, "roughness");
        let target = manifold_ui::GraphParamTarget::Generator;

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), 0.9), &mut project);
        h.drain();
        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let ContentCommand::Execute(mut cmd) = h.drain().into_iter().next().unwrap() else {
            panic!("expected Execute");
        };
        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst_def = layer.generator_graph().unwrap();
        let group = inst_def.nodes.iter().find(|n| n.id == group_node_id).unwrap();
        let body = group.group.as_ref().unwrap();
        let node = body.nodes.iter().find(|n| n.id == mat_node_doc_id).unwrap();
        match node.params.get("roughness") {
            Some(SerializedParamValue::Float { value }) => {
                assert!((value - 0.9).abs() < 1e-4, "instance def must carry the committed roughness, got {value}");
            }
            other => panic!("expected a Float roughness in the group-scoped instance def, got {other:?}"),
        }
    }

    /// C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): fresh SceneStarter's
    /// own Light/Camera node ids — bundled with the layer, never grouped
    /// (unlike Objects — `state_sync`'s Light/Camera `row` closure always
    /// synthesizes `RowAddr::root`, so unlike C-P1b's Object-family bug there
    /// is no group-scope case to prove here; these families' commits land at
    /// root scope by construction).
    fn scene_layer_starter_light_and_camera() -> (Project, LayerId, u32, u32) {
        let (project, layer_id) = scene_layer_project();
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let def = layer.generator_graph().cloned().unwrap_or_else(|| {
            manifold_renderer::node_graph::bundled_preset_def(&layer.generator_type().clone())
                .cloned()
                .expect("SceneStarter is a bundled preset")
        });
        let vm = SceneVm::from_def(&def).expect("SceneStarter resolves as a scene");
        let light_node_doc_id = vm
            .lights
            .iter()
            .find_map(|l| match l {
                manifold_renderer::node_graph::scene_vm::SceneLightVm::Known(r) => Some(r.node_doc_id),
                _ => None,
            })
            .expect("SceneStarter ships a Known light");
        let camera_node_doc_id = match &vm.camera {
            manifold_renderer::node_graph::scene_vm::CameraVm::Orbit(c) => c.node_doc_id,
            other => panic!("SceneStarter's camera must be Orbit, got {other:?}"),
        };
        (project, layer_id, light_node_doc_id, camera_node_doc_id)
    }

    /// BUG-237's own render-proof half, at the dispatch level: a Light
    /// Intensity commit through the REAL card-row dispatch path
    /// (`ParamSnapshot`/`ParamChanged`/`ParamCommit`) must land in the
    /// layer's own instance def, at ROOT scope — the mechanism the render-
    /// level PNG proof (session report) shows actually moves pixels.
    #[test]
    fn light_intensity_commit_writes_the_layer_instance_def_at_root_scope() {
        let (mut project, layer_id, light_node_doc_id, _camera_node_doc_id) = scene_layer_starter_light_and_camera();
        let mut h = Harness::new(Some(layer_id.clone()));
        h.ui.scene_setup_panel.open();
        h.ui.scene_setup_panel.set_selection(
            layer_id.clone(),
            manifold_ui::panels::scene_setup_panel::SceneSelection::Light(light_node_doc_id),
        );
        let layer_idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
        super::super::state_sync::sync_inspector_data(&mut h.ui, &project, Some(layer_idx), &manifold_ui::UIState::new(), &[]);
        let mut tree = manifold_ui::tree::UITree::new();
        let dock = manifold_ui::node::Rect::new(0.0, 0.0, 400.0, 800.0);
        let region = tree.begin_region(dock, manifold_ui::ZTier::Base, "scene_setup_test", manifold_ui::node::UIFlags::empty());
        let start = tree.count();
        h.ui.scene_setup_panel.build_docked(&mut tree, dock);
        tree.end_region(region, start);

        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(light_node_doc_id, "intensity");
        let target = manifold_ui::GraphParamTarget::Generator;

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        assert!(h.drain().is_empty(), "Snapshot sends no ContentCommand");
        h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), 6.0), &mut project);
        let live = h.drain();
        assert_eq!(live.len(), 1, "one live tick");
        assert!(
            matches!(live[0], ContentCommand::MutateProjectLive(_)),
            "a motion tick is a live (non-undoable) write, never Execute"
        );
        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let ContentCommand::Execute(mut cmd) = h.drain().into_iter().next().unwrap() else {
            panic!("expected Execute — the commit command must be undo-tracked");
        };
        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst_def = layer.generator_graph().expect("the write must land in the layer's OWN instance def");
        let node = inst_def
            .nodes
            .iter()
            .find(|n| n.id == light_node_doc_id)
            .expect("the light node must be found at ROOT scope — never grouped");
        match node.params.get("intensity") {
            Some(SerializedParamValue::Float { value }) => {
                assert!((value - 6.0).abs() < 1e-4, "instance def must carry the committed intensity, got {value}");
            }
            other => panic!("expected a Float intensity in the root-scoped instance def, got {other:?}"),
        }

        cmd.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst_def = layer.generator_graph();
        let still_overridden = inst_def
            .and_then(|d| d.nodes.iter().find(|n| n.id == light_node_doc_id))
            .and_then(|n| n.params.get("intensity"))
            .is_some_and(|v| matches!(v, SerializedParamValue::Float { value } if (value - 6.0).abs() < 1e-4));
        assert!(!still_overridden, "undo must restore the pre-drag intensity — one undo unit per gesture");
    }

    /// C-P1c: the Camera family's twin — proves the commit path AND D10's
    /// degrees-display contract (`is_angle`) don't interfere with each other:
    /// `ParamChanged`/`ParamCommit` carry the RAW RADIANS value throughout
    /// (the degrees conversion is a display/drag-scaling concern that lives
    /// entirely inside `build_camera_card_row`'s `ParamInfo.is_angle` and the
    /// UI-level drag math, never the dispatched payload).
    #[test]
    fn camera_orbit_commit_writes_the_layer_instance_def_at_root_scope() {
        let (mut project, layer_id, _light_node_doc_id, camera_node_doc_id) = scene_layer_starter_light_and_camera();
        let mut h = Harness::new(Some(layer_id.clone()));
        h.ui.scene_setup_panel.open();
        h.ui.scene_setup_panel
            .set_selection(layer_id.clone(), manifold_ui::panels::scene_setup_panel::SceneSelection::Camera);
        let layer_idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
        super::super::state_sync::sync_inspector_data(&mut h.ui, &project, Some(layer_idx), &manifold_ui::UIState::new(), &[]);
        let mut tree = manifold_ui::tree::UITree::new();
        let dock = manifold_ui::node::Rect::new(0.0, 0.0, 400.0, 800.0);
        let region = tree.begin_region(dock, manifold_ui::ZTier::Base, "scene_setup_test", manifold_ui::node::UIFlags::empty());
        let start = tree.count();
        h.ui.scene_setup_panel.build_docked(&mut tree, dock);
        tree.end_region(region, start);

        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(camera_node_doc_id, "orbit");
        let target = manifold_ui::GraphParamTarget::Generator;
        let new_orbit_radians = 1.2_f32;

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), new_orbit_radians), &mut project);
        h.drain();
        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let ContentCommand::Execute(mut cmd) = h.drain().into_iter().next().unwrap() else {
            panic!("expected Execute");
        };
        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst_def = layer.generator_graph().expect("the write must land in the layer's OWN instance def");
        let node = inst_def
            .nodes
            .iter()
            .find(|n| n.id == camera_node_doc_id)
            .expect("the camera node must be found at ROOT scope — never grouped");
        match node.params.get("orbit") {
            Some(SerializedParamValue::Float { value }) => {
                assert!(
                    (value - new_orbit_radians).abs() < 1e-4,
                    "instance def must carry the RAW RADIANS commit value ({new_orbit_radians}), got {value} — \
                     is_angle is a display concern, never a storage conversion"
                );
            }
            other => panic!("expected a Float orbit in the root-scoped instance def, got {other:?}"),
        }
    }

    /// Root-fix regression (2026-07-18, Peter: "cameras … don't work from
    /// the scene controls"): the glb importer card-binds its camera
    /// (`cam_orbit` → `camera.orbit`), and a def write on a BOUND param is
    /// structurally dead — the chain rebuild re-seeds the binding's value
    /// over it. A Scene Setup camera drag on an imported layer must
    /// therefore edit the binding's INSTANCE SLOT (`cam_orbit`), the same
    /// value the perform card drives, and never the def node param.
    #[test]
    fn imported_scene_camera_drag_writes_the_bound_card_slot_not_the_def() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
        let (def, _report) =
            manifold_renderer::node_graph::gltf_import::assemble_import_graph(&fixture)
                .expect("assemble azalea");
        let camera_node_doc_id = match SceneVm::from_def(&def).expect("import resolves as a scene").camera {
            manifold_renderer::node_graph::scene_vm::CameraVm::Orbit(c) => c.node_doc_id,
            other => panic!("importer camera must be Orbit, got {other:?}"),
        };

        // Install exactly the way `finish_import_model` does: embedded
        // preset + overlay first (so `init_defaults` seeds `cam_orbit`),
        // then the production layer command.
        let mut project = Project::default();
        let embedded = manifold_core::project::EmbeddedPreset {
            kind: manifold_core::preset_def::PresetKind::Generator,
            def,
            origin: manifold_core::project::EmbeddedOrigin::Saved,
        };
        project.upsert_embedded_preset(embedded.clone());
        crate::project_io::install_project_preset_overlay(&project);
        let mut layer_cmd = manifold_editing::commands::layer::ImportModelLayerCommand::new(
            "Azalea".to_string(),
            embedded,
            0,
            None,
        );
        layer_cmd.execute(&mut project);
        let layer_id = layer_cmd.inserted_layer_id().expect("layer inserted");

        let mut h = Harness::new(Some(layer_id.clone()));
        h.ui.scene_setup_panel.open();
        h.ui.scene_setup_panel
            .set_selection(layer_id.clone(), manifold_ui::panels::scene_setup_panel::SceneSelection::Camera);
        let layer_idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
        super::super::state_sync::sync_inspector_data(&mut h.ui, &project, Some(layer_idx), &manifold_ui::UIState::new(), &[]);
        let mut tree = manifold_ui::tree::UITree::new();
        let dock = manifold_ui::node::Rect::new(0.0, 0.0, 400.0, 800.0);
        let region = tree.begin_region(dock, manifold_ui::ZTier::Base, "scene_setup_test", manifold_ui::node::UIFlags::empty());
        let start = tree.count();
        h.ui.scene_setup_panel.build_docked(&mut tree, dock);
        tree.end_region(region, start);

        let pid = manifold_ui::panels::scene_setup_panel::synth_world_param_id(camera_node_doc_id, "orbit");
        let target = manifold_ui::GraphParamTarget::Generator;
        let new_orbit = 1.9_f32;

        h.dispatch(&PanelAction::ParamSnapshot(target, pid.clone()), &mut project);
        h.dispatch(&PanelAction::ParamChanged(target, pid.clone(), new_orbit), &mut project);
        h.drain();

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let inst = layer.gen_params().expect("imported layer carries a PresetInstance");
        assert!(
            inst.params.contains("cam_orbit"),
            "importer metadata must expose the cam_orbit slot"
        );
        assert!(
            (inst.get_base_param("cam_orbit") - new_orbit).abs() < 1e-4,
            "the drag must move the BOUND card slot (cam_orbit), got {}",
            inst.get_base_param("cam_orbit")
        );
        // The def node param must be untouched — the slot is the one value.
        let def_untouched = layer
            .generator_graph()
            .and_then(|d| d.nodes.iter().find(|n| n.id == camera_node_doc_id))
            .and_then(|n| n.params.get("orbit"))
            .is_none_or(|v| !matches!(v, SerializedParamValue::Float { value } if (value - new_orbit).abs() < 1e-4));
        assert!(def_untouched, "a bound row's write must never land in the def");

        h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);
        let ContentCommand::Execute(mut cmd) = h.drain().into_iter().next().expect("commit dispatches") else {
            panic!("expected an undo-tracked Execute commit");
        };
        cmd.execute(&mut project);
        cmd.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let restored = layer.gen_params().unwrap().get_base_param("cam_orbit");
        assert!(
            (restored - new_orbit).abs() > 1e-4,
            "undo must restore the pre-drag slot value, still at {restored}"
        );
    }
}
