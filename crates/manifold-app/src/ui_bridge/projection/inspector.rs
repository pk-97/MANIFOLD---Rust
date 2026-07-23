//! Inspector projection: the big `sync_inspector_data` selection sync, the
//! per-card modulation builders, single-param modulation lookups, and audio/
//! modifier label helpers. Moved from state_sync.rs (P-P,
//! UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use manifold_core::PresetTypeId;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, LayerType};
use manifold_ui::panels::param_card::RowMod;
use manifold_ui::panels::param_slider_shared::{AudioCardState, AudioRowState};
use manifold_ui::param_surface::ParamSurface;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

use super::cards::{
    OscScope, SurfaceVisibility, attach_audio_sends, effects_to_surfaces, gen_params_to_surface,
};
use super::scene::sections_for_doc_ids;

/// Sync inspector content for the active selection.
/// Called when the active layer changes or after structural mutations.
pub fn sync_inspector_data(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) {
    // Audio Setup modal — refresh its current device + send list while it's
    // open. Resolving the device through the directory once per sync (only while
    // the modal is up) gives each send row its real channel name, grouped or
    // not, instead of a bare index.
    if ui.audio_setup_panel.is_open() {
        use manifold_core::AudioSourceKind;
        use manifold_ui::panels::audio_setup_panel::AudioSendRow;
        let dir = manifold_audio::directory::system_directory();
        // Tap sources (system / app output) don't live in the input-device list,
        // so resolving them there would always read "missing". Only resolve a
        // hardware device; a tap's liveness is checked separately below.
        let is_tap = project.audio_setup.device.as_ref().is_some_and(|d| d.is_tap());
        let device = match &project.audio_setup.device {
            Some(d) if !d.is_tap() => dir.resolve(d.uid_opt(), Some(&d.name)),
            Some(_) => None, // a tap — no DeviceInfo
            None => dir.list_input_devices().into_iter().find(|d| d.is_default),
        };
        let sends = project
            .audio_setup
            .sends
            .iter()
            .map(|s| {
                // Read-only source view: the full routing lines (capture device +
                // each feeding layer) for the Inputs section. Routing is edited
                // elsewhere — layers from the layer header, channels from the
                // channel control (the row-level "Cap" chip and its click-to-reveal
                // dropdown are gone; this is the one place the detail lives now).
                let ch_label = channel_label(device.as_ref(), is_tap, &s.channels);
                let cap = s.has_capture();
                let layer_name = |lid: &manifold_core::LayerId| {
                    project
                        .timeline
                        .layers
                        .iter()
                        .find(|l| &l.layer_id == lid)
                        .map(|l| l.name.clone())
                        .unwrap_or_else(|| {
                            manifold_ui::panels::audio_setup_panel::MISSING_LAYER_LABEL.to_string()
                        })
                };
                // Full routing lines for the read-only Inputs section.
                let mut routings: Vec<String> = Vec::new();
                if cap {
                    routings.push(format!("Capture \u{2022} {ch_label}"));
                }
                for lid in s.layers() {
                    routings.push(format!("Layer \u{2022} {}", layer_name(lid)));
                }
                // Consumers section: named audio mods (param gate/continuous
                // cards) plus enabled layer-owned `LayerClipTrigger` configs
                // (P3, D2 — the matrix's per-band route walk is gone; clip
                // triggers are authored on the layer only). Both are purely
                // navigational rows (D3): click selects the owning layer.
                let clip_triggers = project.clip_trigger_consumers(&s.id);
                let has_clip_triggers = !clip_triggers.is_empty();
                let consumers: Vec<manifold_ui::panels::audio_setup_panel::SendConsumerRow> =
                    project
                        .audio_mod_consumers(&s.id)
                        .into_iter()
                        .chain(clip_triggers)
                        .map(|(layer_id, label)| {
                            manifold_ui::panels::audio_setup_panel::SendConsumerRow { label, layer_id }
                        })
                        .collect();
                // Inputs section: audio layers feeding this send (id + name).
                let feeding_layers: Vec<(manifold_core::LayerId, String)> =
                    s.layers().iter().map(|lid| (lid.clone(), layer_name(lid))).collect();

                AudioSendRow {
                    id: s.id.clone(),
                    label: s.label.clone(),
                    channel_label: ch_label,
                    channels: s.channels.clone(),
                    gain_db: s.gain_db,
                    floor_db: s.floor_db,
                    driven_count: project.audio_send_usage_count(&s.id),
                    routings,
                    has_clip_triggers,
                    feeding_layers,
                    consumers,
                }
            })
            .collect();

        // Surface a reliability warning: a chosen source that can't capture right
        // now — a device that won't resolve / reads offline, a tap on an OS that
        // can't tap, an app that isn't running — else a blocked mic permission.
        let status_warning = match &project.audio_setup.device {
            Some(d) => match d.kind {
                AudioSourceKind::InputDevice => match &device {
                    None => Some(format!("\u{26A0} \"{}\" is offline or unplugged", d.name)),
                    Some(info) if !info.is_alive => {
                        Some(format!("\u{26A0} \"{}\" is offline", info.name))
                    }
                    _ => None,
                },
                AudioSourceKind::SystemAudio => (!dir.tap_capabilities().system_audio)
                    .then(|| "\u{26A0} System audio capture needs macOS 14.4+".to_string()),
                AudioSourceKind::App => dir
                    .resolve_app(d.uid_opt().unwrap_or(""))
                    .is_none()
                    .then(|| format!("\u{26A0} \"{}\" isn't running", d.name)),
            },
            None => None,
        }
        .or_else(|| {
            (!manifold_audio::permission::status().is_usable())
                .then(|| "\u{26A0} Microphone access blocked — check System Settings".to_string())
        });

        ui.audio_setup_panel.configure(
            project
                .audio_setup
                .device
                .as_ref()
                .map(crate::ui_translate::audio_device_ref_to_ui),
            sends,
            status_warning,
        );

    }

    // ── Scene Setup panel (SCENE_SETUP_PANEL_DESIGN.md) ──
    // Rebuilt from scratch every sync while the dock is open — no cached/
    // staged copy anywhere (D1: "no rotting, no staleness"). Selection
    // scoping mirrors the inspector-tab rung derivation just below (§1 VERIFY
    // marker, resolved): the selection's own layer, falling back to
    // `active_layer`.
    if ui.scene_setup_panel.is_open() {
        use manifold_renderer::node_graph::scene_vm::{SceneVm, is_param_driven, is_param_exposed};
        use manifold_ui::panels::scene_setup_panel::{
            AtmosphereRowVm, EnvironmentRowVm, ObjectMaterialVm, ObjectRowVm, RowAddr, RowValue,
            SceneSetupState, SceneSetupVm, TransformRowVm,
        };

        let sel_layer_idx = selection
            .selected_layer_id_for_clip
            .as_ref()
            .or(selection.primary_selected_layer_id.as_ref())
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .or(active_layer);
        let layer = sel_layer_idx.and_then(|i| project.timeline.layers.get(i));

        // P2 slice 2a: the scene panel's bound layer's FULL generator
        // `ParamSurface`, filled in below only in the `Live` arm — see
        // `ScenePanel::configure_params`'s doc comment.
        let mut full_params: Option<ParamSurface> = None;
        let state = match layer {
            None => SceneSetupState::NoSelection("Select a layer to set up its scene.".to_string()),
            Some(l) if l.layer_type != LayerType::Generator => SceneSetupState::NoSelection(
                "Select a generator layer to set up its scene.".to_string(),
            ),
            Some(l) => {
                let layer_id = l.layer_id.clone();
                let gen_type = l.generator_type().clone();
                if gen_type.is_none() {
                    SceneSetupState::NoGenerator { layer_id }
                } else {
                    let def = l
                        .generator_graph()
                        .cloned()
                        .or_else(|| manifold_renderer::node_graph::bundled_preset_def(&gen_type).cloned());
                    match def.as_ref().and_then(SceneVm::from_def) {
                        None => SceneSetupState::NoScene { layer_id },
                        Some(vm) => {
                            // Ranges transcribed from each primitive's own
                            // `ParamDef::range` (`bake_environment`'s
                            // intensity [0,4] / fill [0,2]; `atmosphere`'s
                            // fog_density [0,1] / height_falloff [0,2]).
                            // UX-P3a: `exposed` is a free read off the SAME
                            // `def` `SceneVm::from_def` just walked
                            // (`is_param_exposed` — a second independent
                            // O(nodes) pass, node doc ids unique
                            // document-wide) — every row built through `row`/
                            // `scoped_row` gets a correct lit state for free,
                            // not just the rows P3a actually wires a mod
                            // button onto.
                            // Bound-row value override: a row whose inner (node, param) is
                            // covered by a card/user binding LIVES in the
                            // binding's instance slot — the write path edits
                            // that slot, so the displayed value must read it
                            // too, or the panel shows the def's stale import
                            // default.
                            let hoisted_gen_inst = l.gen_params();
                            let display_value = |node_doc_id: u32, param_id: &str, fallback: f32| {
                                hoisted_gen_inst
                                    .and_then(|inst| {
                                        // Instance graph first; a TRACKING
                                        // instance (graph: None — fresh
                                        // imports) resolves via the same
                                        // effective def the VM was built on.
                                        let id = inst
                                            .binding_id_for_node_param(node_doc_id, param_id)
                                            .or_else(|| {
                                                manifold_core::effects::binding_id_for_node_param_in(
                                                    def.as_ref()?,
                                                    node_doc_id,
                                                    param_id,
                                                )
                                            })?;
                                        inst.params
                                            .contains(id.as_str())
                                            .then(|| inst.get_base_param(id.as_str()))
                                    })
                                    .unwrap_or(fallback)
                            };
                            // P3 (scene_vm slimming): `is_param_driven` is the
                            // sole source of every row's driven-state now —
                            // the per-struct `_driven` fields scene_vm used to
                            // transcribe are gone; this wraps the shared
                            // helper against the SAME `def` `display_value`
                            // already closes over.
                            let is_driven = |node_doc_id: u32, param_id: &str| {
                                def.as_ref().is_some_and(|d| is_param_driven(d, node_doc_id, param_id))
                            };
                            let row = |node_doc_id: u32,
                                       param_id: &str,
                                       value: f32,
                                       driven: bool,
                                       min: f32,
                                       max: f32| RowValue {
                                addr: RowAddr::root(node_doc_id, param_id),
                                value: display_value(node_doc_id, param_id, value),
                                min,
                                max,
                                driven,
                                exposed: def
                                    .as_ref()
                                    .is_some_and(|d| is_param_exposed(d, node_doc_id, param_id)),
                            };
                            // Scoped variant for a P2 Objects row living
                            // inside the object's own group (material/
                            // modifier params) — same shape, plus the
                            // `[group_node_id]` scope the graph command
                            // family's `.with_scope` takes.
                            let scoped_row = |scope_path: Vec<u32>,
                                              node_doc_id: u32,
                                              param_id: &str,
                                              value: f32,
                                              driven: bool,
                                              min: f32,
                                              max: f32| RowValue {
                                addr: RowAddr { scope_path, node_doc_id, param_id: param_id.to_string() },
                                value: display_value(node_doc_id, param_id, value),
                                min,
                                max,
                                driven,
                                exposed: def
                                    .as_ref()
                                    .is_some_and(|d| is_param_exposed(d, node_doc_id, param_id)),
                            };
                            // C-P1b (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md):
                            // moved up from its original C-P1a position
                            // (right before the old Environment/Fog-only
                            // `environment`/`atmosphere` construction below)
                            // so `transform_row`/`material_row` — which build
                            // BEFORE that point in this match arm — can also
                            // wrap their rows in `ModulatedRow` via `mrow`.
                            // Same closure, same definition, just visible
                            // earlier in this block; the Environment/Fog call
                            // sites further down are unchanged.
                            use manifold_ui::panels::scene_setup_panel::ModulatedRow;
                            let gen_inst = l.gen_params();
                            // BUG-249: modulation facts resolve through the
                            // REAL exposed-param binding id, not the row's
                            // synthesized scene id (which the runtime never
                            // evaluates) — see `scene_row_modulation`.
                            let mrow = |node_doc_id: u32, param_key: &str, v: RowValue| ModulatedRow {
                                modulation: Box::new(scene_row_modulation(
                                    gen_inst,
                                    def.as_ref(),
                                    node_doc_id,
                                    param_key,
                                    automation_latched,
                                )),
                                value: v,
                            };
                            let transform_row = |t: &manifold_renderer::node_graph::scene_vm::TransformVm| {
                                // D12 fix: `t`'s own addresses already carry
                                // the correct `scope_path` (empty for a
                                // root/ungrouped atom, `[group_node_id]` for
                                // one living inside an object's group) — use
                                // it directly instead of the old `row()`
                                // (root-only) helper, which silently wrote
                                // to the wrong scope for any grouped
                                // object's transform.
                                let scope = t.pos_addr.0.scope_path.clone();
                                let row = |node_doc_id: u32, param_id: &str, value: f32, driven: bool, min: f32, max: f32| {
                                    scoped_row(scope.clone(), node_doc_id, param_id, value, driven, min, max)
                                };
                                // C-P1b: each cell is now a `ModulatedRow` —
                                // `mrow` synthesizes the SAME
                                // `scene.{node_doc_id}.{param_key}` id the
                                // panel's `build_object_card_row` uses to key
                                // its own id map (D2's "one definition both
                                // sides use"), independent of `scope_path`
                                // (node_doc_id alone is document-wide unique,
                                // so a grouped object's transform still
                                // resolves its modulation facts correctly).
                                Box::new(TransformRowVm {
                                    pos: (
                                        mrow(t.node_doc_id, "pos_x", row(t.node_doc_id, "pos_x", t.pos_value.0, t.pos_driven.0, -100.0, 100.0)),
                                        mrow(t.node_doc_id, "pos_y", row(t.node_doc_id, "pos_y", t.pos_value.1, t.pos_driven.1, -100.0, 100.0)),
                                        mrow(t.node_doc_id, "pos_z", row(t.node_doc_id, "pos_z", t.pos_value.2, t.pos_driven.2, -100.0, 100.0)),
                                    ),
                                    rot: (
                                        mrow(
                                            t.node_doc_id,
                                            "rot_x",
                                            row(
                                                t.node_doc_id,
                                                "rot_x",
                                                t.rot_value.0,
                                                t.rot_driven.0,
                                                -std::f32::consts::TAU,
                                                std::f32::consts::TAU,
                                            ),
                                        ),
                                        mrow(
                                            t.node_doc_id,
                                            "rot_y",
                                            row(
                                                t.node_doc_id,
                                                "rot_y",
                                                t.rot_value.1,
                                                t.rot_driven.1,
                                                -std::f32::consts::TAU,
                                                std::f32::consts::TAU,
                                            ),
                                        ),
                                        mrow(
                                            t.node_doc_id,
                                            "rot_z",
                                            row(
                                                t.node_doc_id,
                                                "rot_z",
                                                t.rot_value.2,
                                                t.rot_driven.2,
                                                -std::f32::consts::TAU,
                                                std::f32::consts::TAU,
                                            ),
                                        ),
                                    ),
                                    scale: (
                                        mrow(t.node_doc_id, "scale_x", row(t.node_doc_id, "scale_x", t.scale_value.0, t.scale_driven.0, 0.01, 10.0)),
                                        mrow(t.node_doc_id, "scale_y", row(t.node_doc_id, "scale_y", t.scale_value.1, t.scale_driven.1, 0.01, 10.0)),
                                        mrow(t.node_doc_id, "scale_z", row(t.node_doc_id, "scale_z", t.scale_value.2, t.scale_driven.2, 0.01, 10.0)),
                                    ),
                                })
                            };
                            let material_row =
                                |m: &manifold_renderer::node_graph::scene_vm::MaterialVm| match m
                                {
                                    manifold_renderer::node_graph::scene_vm::MaterialVm::Known(row_data) => {
                                        // D12 fix: `row_data.scope_path`
                                        // already carries the correct scope
                                        // (see `transform_row`'s identical
                                        // fix above) — no external
                                        // group_node_id needed, and it's
                                        // correct for an ungrouped object
                                        // too (empty scope).
                                        let scope = row_data.scope_path.clone();
                                        // C-P1b: `ModulatedRow`s, same
                                        // `mrow` synthesis as `transform_row`
                                        // above. Values/driven-state are P3
                                        // manifest reads keyed on the same
                                        // node id — the struct only carries
                                        // identity now.
                                        let color = (
                                            mrow(row_data.node_doc_id, "color_r", scoped_row(
                                                scope.clone(),
                                                row_data.node_doc_id,
                                                "color_r",
                                                0.8,
                                                is_driven(row_data.node_doc_id, "color_r"),
                                                0.0,
                                                1.0,
                                            )),
                                            mrow(row_data.node_doc_id, "color_g", scoped_row(
                                                scope.clone(),
                                                row_data.node_doc_id,
                                                "color_g",
                                                0.8,
                                                is_driven(row_data.node_doc_id, "color_g"),
                                                0.0,
                                                1.0,
                                            )),
                                            mrow(row_data.node_doc_id, "color_b", scoped_row(
                                                scope.clone(),
                                                row_data.node_doc_id,
                                                "color_b",
                                                0.8,
                                                is_driven(row_data.node_doc_id, "color_b"),
                                                0.0,
                                                1.0,
                                            )),
                                        );
                                        if row_data.is_pbr {
                                            ObjectMaterialVm::Pbr {
                                                color,
                                                metallic: mrow(row_data.node_doc_id, "metallic", scoped_row(
                                                    scope.clone(),
                                                    row_data.node_doc_id,
                                                    "metallic",
                                                    0.0,
                                                    is_driven(row_data.node_doc_id, "metallic"),
                                                    0.0,
                                                    1.0,
                                                )),
                                                roughness: mrow(row_data.node_doc_id, "roughness", scoped_row(
                                                    scope,
                                                    row_data.node_doc_id,
                                                    "roughness",
                                                    0.5,
                                                    is_driven(row_data.node_doc_id, "roughness"),
                                                    0.01,
                                                    1.0,
                                                )),
                                            }
                                        } else {
                                            ObjectMaterialVm::Other { color }
                                        }
                                    }
                                    manifold_renderer::node_graph::scene_vm::MaterialVm::None => {
                                        ObjectMaterialVm::None
                                    }
                                };
                            let objects: Vec<ObjectRowVm> = vm
                                .objects
                                .iter()
                                .map(|o| match o {
                                    manifold_renderer::node_graph::scene_vm::SceneObjectVm::Known(known) => {
                                        let manifold_renderer::node_graph::scene_vm::SceneObjectKnownRow {
                                            index,
                                            object_node_id,
                                            group_node_id,
                                            name,
                                            visible_addr,
                                            visible_value,
                                            visible_driven,
                                            transform,
                                            material,
                                            modifier_chain,
                                            modifier_chain_parseable,
                                            ..
                                        } = known.as_ref();
                                        // P2 slice 2a: the real P1 section
                                        // string(s) covering this object —
                                        // its scene_object node, transform
                                        // node, material node, and every
                                        // modifier in its stack. Read
                                        // straight off the layer's exposure
                                        // metadata via doc-id cross-reference
                                        // (`sections_for_doc_ids`) — never
                                        // reconstructed from a naming
                                        // convention (creation-time and
                                        // load-migration stamping produce
                                        // different strings for the same
                                        // node kind).
                                        let mut object_doc_ids = vec![*object_node_id];
                                        if let Some(t) = transform {
                                            object_doc_ids.push(t.node_doc_id);
                                        }
                                        if let manifold_renderer::node_graph::scene_vm::MaterialVm::Known(m) =
                                            material
                                        {
                                            object_doc_ids.push(m.node_doc_id);
                                        }
                                        object_doc_ids.extend(modifier_chain.iter().map(|m| m.node_doc_id));
                                        let sections = sections_for_doc_ids(def.as_ref(), &object_doc_ids);
                                        ObjectRowVm::Known(Box::new(
                                            manifold_ui::panels::scene_setup_panel::ObjectKnownRow {
                                                index: *index,
                                                object_node_id: *object_node_id,
                                                group_node_id: *group_node_id,
                                                name: name.clone(),
                                                visible: scoped_row(
                                                    visible_addr.scope_path.clone(),
                                                    visible_addr.node_doc_id,
                                                    &visible_addr.param_id,
                                                    if *visible_value { 1.0 } else { 0.0 },
                                                    *visible_driven,
                                                    0.0,
                                                    1.0,
                                                ),
                                                transform: transform.as_ref().map(&transform_row),
                                                material: material_row(material),
                                                modifiers: modifier_chain
                                                    .iter()
                                                    .enumerate()
                                                    .map(|(i, m)| manifold_ui::panels::scene_setup_panel::ModifierKnownRow {
                                                        index: i,
                                                        node_doc_id: m.node_doc_id,
                                                        display_name: modifier_display_name(&m.type_id),
                                                    })
                                                    .collect(),
                                                modifiers_addable: *modifier_chain_parseable,
                                                sections,
                                            },
                                        ))
                                    }
                                    manifold_renderer::node_graph::scene_vm::SceneObjectVm::Custom { index } => {
                                        ObjectRowVm::Custom { index: *index }
                                    }
                                })
                                .collect();
                            // P3: Lights + Camera. Enum-label arrays
                            // transcribed from `node.light`'s own
                            // `LIGHT_MODES`/`SHADOW_SOFTNESS_LABELS`
                            // constants (`light.rs`) — this crate can't
                            // depend on them directly through the UI DTO
                            // boundary (`manifold-ui` doesn't depend on
                            // `manifold-renderer`), same convention as
                            // `EnvironmentRowVm::mode_is_hdri`.
                            const LIGHT_MODE_LABELS: &[&str] = &["Sun", "Point"];
                            const SHADOW_SOFTNESS_LABELS: &[&str] = &["Hard", "Soft", "VerySoft", "Contact"];
                            const CAST_SHADOWS_LABELS: &[&str] = &["Off", "On"];
                            // C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md):
                            // Light's Mode/Cast Shadows/Shadow Softness rows
                            // are now `ModulatedEnumRow` — same `mrow`
                            // promotion `transform_row`/`material_row` above
                            // already do for their own fields.
                            let enum_row = |node_doc_id: u32,
                                            param_id: &str,
                                            value: u32,
                                            driven: bool,
                                            labels: &'static [&'static str]| {
                                manifold_ui::panels::scene_setup_panel::ModulatedEnumRow {
                                    row: mrow(
                                        node_doc_id,
                                        param_id,
                                        row(node_doc_id, param_id, value as f32, driven, 0.0, (labels.len() - 1) as f32),
                                    ),
                                    labels: labels.to_vec(),
                                }
                            };
                            let lights: Vec<manifold_ui::panels::scene_setup_panel::LightRowVm> = vm
                                .lights
                                .iter()
                                .map(|l| match l {
                                    manifold_renderer::node_graph::scene_vm::SceneLightVm::Known(r) => {
                                        manifold_ui::panels::scene_setup_panel::LightRowVm::Known(Box::new(
                                            manifold_ui::panels::scene_setup_panel::LightKnownRow {
                                                index: r.index,
                                                node_doc_id: r.node_doc_id,
                                                name: r.name.clone(),
                                                mode: enum_row(r.node_doc_id, "mode", 0, is_driven(r.node_doc_id, "mode"), LIGHT_MODE_LABELS),
                                                color: (
                                                    mrow(r.node_doc_id, "color_r", row(r.node_doc_id, "color_r", 1.0, is_driven(r.node_doc_id, "color_r"), 0.0, 1.0)),
                                                    mrow(r.node_doc_id, "color_g", row(r.node_doc_id, "color_g", 1.0, is_driven(r.node_doc_id, "color_g"), 0.0, 1.0)),
                                                    mrow(r.node_doc_id, "color_b", row(r.node_doc_id, "color_b", 1.0, is_driven(r.node_doc_id, "color_b"), 0.0, 1.0)),
                                                ),
                                                intensity: mrow(r.node_doc_id, "intensity", row(
                                                    r.node_doc_id,
                                                    "intensity",
                                                    1.0,
                                                    is_driven(r.node_doc_id, "intensity"),
                                                    0.0,
                                                    10.0,
                                                )),
                                                pos: (
                                                    mrow(r.node_doc_id, "pos_x", row(r.node_doc_id, "pos_x", 0.0, is_driven(r.node_doc_id, "pos_x"), -100.0, 100.0)),
                                                    mrow(r.node_doc_id, "pos_y", row(r.node_doc_id, "pos_y", 30.0, is_driven(r.node_doc_id, "pos_y"), -100.0, 100.0)),
                                                    mrow(r.node_doc_id, "pos_z", row(r.node_doc_id, "pos_z", 0.0, is_driven(r.node_doc_id, "pos_z"), -100.0, 100.0)),
                                                ),
                                                aim: (
                                                    mrow(r.node_doc_id, "aim_x", row(r.node_doc_id, "aim_x", 0.0, is_driven(r.node_doc_id, "aim_x"), -100.0, 100.0)),
                                                    mrow(r.node_doc_id, "aim_y", row(r.node_doc_id, "aim_y", 0.0, is_driven(r.node_doc_id, "aim_y"), -100.0, 100.0)),
                                                    mrow(r.node_doc_id, "aim_z", row(r.node_doc_id, "aim_z", 0.0, is_driven(r.node_doc_id, "aim_z"), -100.0, 100.0)),
                                                ),
                                                cast_shadows: enum_row(
                                                    r.node_doc_id,
                                                    "cast_shadows",
                                                    1,
                                                    is_driven(r.node_doc_id, "cast_shadows"),
                                                    CAST_SHADOWS_LABELS,
                                                ),
                                                shadow_softness: enum_row(
                                                    r.node_doc_id,
                                                    "shadow_softness",
                                                    1,
                                                    is_driven(r.node_doc_id, "shadow_softness"),
                                                    SHADOW_SOFTNESS_LABELS,
                                                ),
                                                light_size: mrow(r.node_doc_id, "light_size", row(
                                                    r.node_doc_id,
                                                    "light_size",
                                                    1.0,
                                                    is_driven(r.node_doc_id, "light_size"),
                                                    0.0,
                                                    20.0,
                                                )),
                                                // P2 slice 2a: see
                                                // `ObjectKnownRow::sections`'s
                                                // doc comment.
                                                sections: sections_for_doc_ids(def.as_ref(), &[r.node_doc_id]),
                                            },
                                        ))
                                    }
                                    manifold_renderer::node_graph::scene_vm::SceneLightVm::Custom { index } => {
                                        manifold_ui::panels::scene_setup_panel::LightRowVm::Custom { index: *index }
                                    }
                                })
                                .collect();
                            let lens_row = |l: &manifold_renderer::node_graph::scene_vm::LensRow| {
                                manifold_ui::panels::scene_setup_panel::LensRowVm {
                                    focus_distance: mrow(l.node_doc_id, "focus_distance", row(
                                        l.node_doc_id,
                                        "focus_distance",
                                        0.0,
                                        is_driven(l.node_doc_id, "focus_distance"),
                                        0.0,
                                        1000.0,
                                    )),
                                    f_stop: mrow(l.node_doc_id, "f_stop", row(l.node_doc_id, "f_stop", 1000.0, is_driven(l.node_doc_id, "f_stop"), 0.5, 1000.0)),
                                    shutter_angle: mrow(l.node_doc_id, "shutter_angle", row(
                                        l.node_doc_id,
                                        "shutter_angle",
                                        0.0,
                                        is_driven(l.node_doc_id, "shutter_angle"),
                                        0.0,
                                        360.0,
                                    )),
                                    exposure_ev: mrow(l.node_doc_id, "exposure_ev", row(
                                        l.node_doc_id,
                                        "exposure_ev",
                                        0.0,
                                        is_driven(l.node_doc_id, "exposure_ev"),
                                        -8.0,
                                        8.0,
                                    )),
                                }
                            };
                            let camera = match &vm.camera {
                                manifold_renderer::node_graph::scene_vm::CameraVm::Orbit(c) => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::Orbit(Box::new(
                                        manifold_ui::panels::scene_setup_panel::OrbitCameraRowVm {
                                            orbit: mrow(c.node_doc_id, "orbit", row(c.node_doc_id, "orbit", 0.7, is_driven(c.node_doc_id, "orbit"), -std::f32::consts::TAU, std::f32::consts::TAU)),
                                            tilt: mrow(c.node_doc_id, "tilt", row(c.node_doc_id, "tilt", 0.3, is_driven(c.node_doc_id, "tilt"), -std::f32::consts::TAU, std::f32::consts::TAU)),
                                            distance: mrow(c.node_doc_id, "distance", row(c.node_doc_id, "distance", 4.0, is_driven(c.node_doc_id, "distance"), 0.01, 100.0)),
                                            fov_y: mrow(c.node_doc_id, "fov_y", row(c.node_doc_id, "fov_y", 0.9, is_driven(c.node_doc_id, "fov_y"), 0.05, 2.5)),
                                            lens: c.lens.as_ref().map(lens_row),
                                        },
                                    ))
                                }
                                manifold_renderer::node_graph::scene_vm::CameraVm::Free(c) => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::Free(Box::new(
                                        manifold_ui::panels::scene_setup_panel::FreeCameraRowVm {
                                            pos: (
                                                mrow(c.node_doc_id, "pos_x", row(c.node_doc_id, "pos_x", 0.0, is_driven(c.node_doc_id, "pos_x"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "pos_y", row(c.node_doc_id, "pos_y", 0.0, is_driven(c.node_doc_id, "pos_y"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "pos_z", row(c.node_doc_id, "pos_z", -3.0, is_driven(c.node_doc_id, "pos_z"), -1000.0, 1000.0)),
                                            ),
                                            yaw: mrow(c.node_doc_id, "yaw", row(c.node_doc_id, "yaw", 0.0, is_driven(c.node_doc_id, "yaw"), -std::f32::consts::TAU, std::f32::consts::TAU)),
                                            pitch: mrow(c.node_doc_id, "pitch", row(c.node_doc_id, "pitch", 0.0, is_driven(c.node_doc_id, "pitch"), -1.5, 1.5)),
                                            roll: mrow(c.node_doc_id, "roll", row(c.node_doc_id, "roll", 0.0, is_driven(c.node_doc_id, "roll"), -std::f32::consts::TAU, std::f32::consts::TAU)),
                                            fov_y: mrow(c.node_doc_id, "fov_y", row(c.node_doc_id, "fov_y", 0.9, is_driven(c.node_doc_id, "fov_y"), 0.05, 2.5)),
                                            lens: c.lens.as_ref().map(lens_row),
                                        },
                                    ))
                                }
                                manifold_renderer::node_graph::scene_vm::CameraVm::LookAt(c) => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::LookAt(Box::new(
                                        manifold_ui::panels::scene_setup_panel::LookAtCameraRowVm {
                                            pos: (
                                                mrow(c.node_doc_id, "pos_x", row(c.node_doc_id, "pos_x", 0.0, is_driven(c.node_doc_id, "pos_x"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "pos_y", row(c.node_doc_id, "pos_y", 0.0, is_driven(c.node_doc_id, "pos_y"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "pos_z", row(c.node_doc_id, "pos_z", -3.0, is_driven(c.node_doc_id, "pos_z"), -1000.0, 1000.0)),
                                            ),
                                            target: (
                                                mrow(c.node_doc_id, "target_x", row(c.node_doc_id, "target_x", 0.0, is_driven(c.node_doc_id, "target_x"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "target_y", row(c.node_doc_id, "target_y", 0.0, is_driven(c.node_doc_id, "target_y"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "target_z", row(c.node_doc_id, "target_z", 0.0, is_driven(c.node_doc_id, "target_z"), -1000.0, 1000.0)),
                                            ),
                                            fov_y: mrow(c.node_doc_id, "fov_y", row(c.node_doc_id, "fov_y", 0.9, is_driven(c.node_doc_id, "fov_y"), 0.05, 2.5)),
                                            lens: c.lens.as_ref().map(lens_row),
                                        },
                                    ))
                                }
                                manifold_renderer::node_graph::scene_vm::CameraVm::Custom { .. } => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::Custom
                                }
                                manifold_renderer::node_graph::scene_vm::CameraVm::None => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::None
                                }
                            };
                            // C-P1a (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md
                            // D3): the converted Environment/Fog rows also
                            // need their driver/envelope/audio-mod facts —
                            // this crate is the only side with a
                            // `PresetInstance` to query. `gen_inst`/`mrow`
                            // are defined once, earlier in this match arm
                            // (moved there C-P1b so `transform_row`/
                            // `material_row` can reuse them too) — reused
                            // here unchanged.
                            // P2 slice 2a: the real P1 section string(s)
                            // covering the camera atom (+ lens, if wired)
                            // and World (environment/atmosphere) — see
                            // `SceneSetupVm::camera_sections`/
                            // `world_sections`'s doc comments. Computed from
                            // `vm.camera`/`vm.environment`/`vm.atmosphere`
                            // BEFORE the consuming matches below (reads the
                            // VM's own case analysis, never re-derives graph
                            // topology).
                            let camera_sections = {
                                use manifold_renderer::node_graph::scene_vm::CameraVm;
                                let mut ids = match &vm.camera {
                                    CameraVm::Orbit(c) => vec![c.node_doc_id],
                                    CameraVm::Free(c) => vec![c.node_doc_id],
                                    CameraVm::LookAt(c) => vec![c.node_doc_id],
                                    CameraVm::Custom { .. } | CameraVm::None => Vec::new(),
                                };
                                let lens_id = match &vm.camera {
                                    CameraVm::Orbit(c) => c.lens.as_ref().map(|l| l.node_doc_id),
                                    CameraVm::Free(c) => c.lens.as_ref().map(|l| l.node_doc_id),
                                    CameraVm::LookAt(c) => c.lens.as_ref().map(|l| l.node_doc_id),
                                    CameraVm::Custom { .. } | CameraVm::None => None,
                                };
                                if let Some(id) = lens_id {
                                    ids.push(id);
                                }
                                sections_for_doc_ids(def.as_ref(), &ids)
                            };
                            let world_sections = {
                                use manifold_renderer::node_graph::scene_vm::{AtmosphereVm, EnvironmentVm};
                                let mut ids = Vec::new();
                                // RAYTRACING_DESIGN.md D14/§5.2: the scene
                                // root's stamped "Rendering" section (RT
                                // Enabled / Temporal Upscale) surfaces under
                                // World — scene-global toggles, and World is
                                // the panel's scene-global item.
                                ids.push(vm.scene_root_node_id);
                                match &vm.environment {
                                    EnvironmentVm::Importer(e) => ids.push(e.bake_node_id),
                                    EnvironmentVm::Bare(e) => ids.push(e.node_doc_id),
                                    EnvironmentVm::Custom { .. } | EnvironmentVm::None => {}
                                }
                                if let AtmosphereVm::Wired(a) = &vm.atmosphere {
                                    ids.push(a.node_doc_id);
                                }
                                sections_for_doc_ids(def.as_ref(), &ids)
                            };
                            let environment = match vm.environment {
                                manifold_renderer::node_graph::scene_vm::EnvironmentVm::Importer(e) => {
                                    EnvironmentRowVm::Importer {
                                        // BUG-260's dead-chip case (design
                                        // doc §3b.9): reads through
                                        // `display_value` like every other
                                        // row so a bound "selector" still
                                        // shows correctly; still not a
                                        // clickable RowValue — unchanged
                                        // pre-existing behavior, not this
                                        // lane's scope.
                                        mode_is_hdri: display_value(e.switch_node_id, "selector", 0.0) != 0.0,
                                        intensity: mrow(
                                            e.bake_node_id,
                                            "intensity",
                                            row(
                                                e.bake_node_id,
                                                "intensity",
                                                1.0,
                                                is_driven(e.bake_node_id, "intensity"),
                                                0.0,
                                                4.0,
                                            ),
                                        ),
                                        fill: mrow(
                                            e.bake_node_id,
                                            "fill",
                                            row(
                                                e.bake_node_id,
                                                "fill",
                                                0.0,
                                                is_driven(e.bake_node_id, "fill"),
                                                0.0,
                                                2.0,
                                            ),
                                        ),
                                        hdri_file: e.hdri_file_value,
                                    }
                                }
                                manifold_renderer::node_graph::scene_vm::EnvironmentVm::Bare(e) => {
                                    EnvironmentRowVm::Bare {
                                        intensity: mrow(
                                            e.node_doc_id,
                                            "intensity",
                                            row(
                                                e.node_doc_id,
                                                "intensity",
                                                1.0,
                                                is_driven(e.node_doc_id, "intensity"),
                                                0.0,
                                                4.0,
                                            ),
                                        ),
                                        fill: mrow(
                                            e.node_doc_id,
                                            "fill",
                                            row(
                                                e.node_doc_id,
                                                "fill",
                                                0.0,
                                                is_driven(e.node_doc_id, "fill"),
                                                0.0,
                                                2.0,
                                            ),
                                        ),
                                    }
                                }
                                manifold_renderer::node_graph::scene_vm::EnvironmentVm::Custom { .. } => {
                                    EnvironmentRowVm::Custom
                                }
                                manifold_renderer::node_graph::scene_vm::EnvironmentVm::None => {
                                    EnvironmentRowVm::None
                                }
                            };
                            let atmosphere = match vm.atmosphere {
                                manifold_renderer::node_graph::scene_vm::AtmosphereVm::Wired(a) => {
                                    AtmosphereRowVm::Wired {
                                        density: mrow(
                                            a.node_doc_id,
                                            // BUG-249: the GRAPH param key
                                            // ("fog_density"), not the panel's
                                            // curated row key ("density") —
                                            // `scene_row_modulation` resolves
                                            // the binding by the inner node's
                                            // real param name.
                                            "fog_density",
                                            row(
                                                a.node_doc_id,
                                                "fog_density",
                                                0.0,
                                                is_driven(a.node_doc_id, "fog_density"),
                                                0.0,
                                                1.0,
                                            ),
                                        ),
                                        height_falloff: mrow(
                                            a.node_doc_id,
                                            "height_falloff",
                                            row(
                                                a.node_doc_id,
                                                "height_falloff",
                                                0.0,
                                                is_driven(a.node_doc_id, "height_falloff"),
                                                0.0,
                                                2.0,
                                            ),
                                        ),
                                    }
                                }
                                manifold_renderer::node_graph::scene_vm::AtmosphereVm::None => {
                                    AtmosphereRowVm::None
                                }
                            };
                            let (audio_send_labels, audio_send_ids) = (
                                project.audio_setup.sends.iter().map(|s| s.label.clone()).collect(),
                                project.audio_setup.sends.iter().map(|s| s.id.clone()).collect(),
                            );
                            // P2 slice 2a: the layer's FULL generator
                            // `ParamSurface` — the SAME `gen_params_to_surface`
                            // the main inspector's generator card uses (see
                            // `ScenePanel::configure_params`'s doc comment for
                            // why THIS layer, never `active_layer`).
                            // `All`: the panel filters by SECTION, so it must
                            // see every param — including `card_visible: false`
                            // scale/material rows the curated card hides.
                            full_params = gen_inst.map(|gp| {
                                gen_params_to_surface(
                                    gp,
                                    layer_id.as_str(),
                                    None,
                                    automation_latched,
                                    SurfaceVisibility::All,
                                )
                            });
                            SceneSetupState::Live(Box::new(SceneSetupVm {
                                layer_id,
                                scene_name: l.name.clone(),
                                multiple_scenes: vm.multiple_scenes,
                                object_count: vm.header.object_count,
                                light_count: vm.header.light_count,
                                shadow_caster_count: vm.header.shadow_caster_count,
                                scene_root_node_id: vm.scene_root_node_id,
                                environment,
                                atmosphere,
                                audio_send_labels,
                                audio_send_ids,
                                objects,
                                lights,
                                camera,
                                camera_sections,
                                world_sections,
                            }))
                        }
                    }
                }
            }
        };
        ui.scene_setup_panel.configure(state);
        ui.scene_setup_panel.configure_params(full_params);
    }

    // ── Inspector tabs: the selection's ownership rungs (local→global) ──
    // The rung set is derived from the SELECTION's own layer (the clip's layer
    // or the selected layer), NOT `active_layer` — which now follows the active
    // tab (e.g. it points at the group when the Group rung is pinned). Deriving
    // from the stable selection keeps the full chain available no matter which
    // rung you're viewing. The active rung is the pin if one is set (a tab
    // click), else the selection-derived default.
    {
        use manifold_core::types::LayerType;
        use manifold_ui::InspectorTab;
        let has_clip = selection.primary_selected_clip_id.is_some();
        let sel_layer_idx = selection
            .selected_layer_id_for_clip
            .as_ref()
            .or(selection.primary_selected_layer_id.as_ref())
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .or(active_layer);
        let layer = sel_layer_idx.and_then(|i| project.timeline.layers.get(i));
        let layer_is_group = layer.is_some_and(|l| l.layer_type == LayerType::Group);
        let has_group_parent =
            sel_layer_idx.is_some_and(|i| project.timeline.find_group_parent(i).is_some());

        let mut tabs: Vec<InspectorTab> = Vec::new();
        if has_clip {
            tabs.push(InspectorTab::Clip);
        }
        if let Some(l) = layer {
            if l.layer_type == LayerType::Group {
                tabs.push(InspectorTab::Group);
            } else {
                tabs.push(InspectorTab::Layer);
                if has_group_parent {
                    tabs.push(InspectorTab::Group);
                }
            }
        }
        tabs.push(InspectorTab::Master);

        let active = selection
            .pinned_scope()
            .filter(|t| tabs.contains(t))
            .unwrap_or_else(|| {
                // Default to the LAYER scope on any selection — the layer (its
                // generator, effects, macros) is the persistent thing you tune,
                // so landing there is less jarring than the per-clip view. The
                // Clip tab is still one click away whenever a clip is selected.
                if layer_is_group {
                    InspectorTab::Group
                } else if layer.is_some() {
                    InspectorTab::Layer
                } else if has_clip {
                    InspectorTab::Clip
                } else {
                    InspectorTab::Master
                }
            });
        ui.inspector.configure_tabs(&tabs, active);
    }

    // Master effects → inspector (envelopes ride on each instance)
    let mut master_configs = effects_to_surfaces(
        &project.settings.master_effects,
        OscScope::Master,
        automation_latched,
    );
    attach_audio_sends(&mut master_configs, &project.audio_setup);
    ui.inspector.configure_master_effects(&master_configs);

    // Active layer effects + gen params → inspector
    if let Some(idx) = active_layer {
        if let Some(layer) = project.timeline.layers.get(idx) {
            // AUDIO TRIGGERS (P3b, AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_
            // DESIGN.md) — the layer's own `clip_triggers`, structurally
            // configured alongside gen params/layer effects below. state_sync
            // stays the sole panel data boundary: the inspector never reads
            // `Project` directly. Row label is "{band} → {feature kind}"
            // (the design's own example, "Low → Kick").
            {
                use manifold_ui::panels::audio_trigger_section::{
                    AudioTriggerRowConfig, AudioTriggerSectionConfig,
                };
                let send_labels: Vec<String> =
                    project.audio_setup.sends.iter().map(|s| s.label.clone()).collect();
                let send_ids: Vec<manifold_core::AudioSendId> =
                    project.audio_setup.sends.iter().map(|s| s.id.clone()).collect();
                let rows: Vec<AudioTriggerRowConfig> = layer
                    .clip_triggers
                    .iter()
                    .map(|t| {
                        let feature = t.source.feature;
                        AudioTriggerRowConfig {
                            enabled: t.enabled,
                            label: format!("{} \u{2192} {}", feature.band.label(), feature.kind.label()),
                            kind_idx: feature.kind.index() as i32,
                            band_idx: feature.band.index() as i32,
                            sensitivity: t.shape.sensitivity,
                            send_id: Some(t.source.send_id.clone()),
                            one_shot_beats: t.one_shot_beats.0 as f32,
                        }
                    })
                    .collect();
                ui.inspector.audio_trigger_section_mut().configure(
                    Some(layer.layer_id.clone()),
                    &AudioTriggerSectionConfig { rows, send_labels, send_ids },
                );
            }

            // Layer effects — envelopes ride on each effect instance now.
            let lid = layer.layer_id.as_str();
            let mut layer_effects = layer
                .effects
                .as_ref()
                .map(|e| effects_to_surfaces(e, OscScope::Layer(lid), automation_latched))
                .unwrap_or_default();
            attach_audio_sends(&mut layer_effects, &project.audio_setup);
            ui.inspector
                .configure_layer_effects(&layer_effects, Some(&layer.layer_id));

            // Generator params — find clip's string_params for text fields.
            // Use selected clip if on this layer, otherwise first clip.
            let clip_string_params = selection
                .primary_selected_clip_id
                .as_ref()
                .and_then(|sel_id| layer.clips.iter().find(|c| c.id == *sel_id))
                .or_else(|| layer.clips.first())
                .and_then(|c| c.string_params.as_ref());
            let mut gen_config = layer
                .gen_params()
                .filter(|gp| *gp.generator_type() != PresetTypeId::NONE)
                .map(|gp| {
                    gen_params_to_surface(
                        gp,
                        lid,
                        clip_string_params,
                        automation_latched,
                        // The main inspector's generator CARD is curated.
                        SurfaceVisibility::CuratedCard,
                    )
                });
            if let Some(c) = gen_config.as_mut() {
                attach_audio_sends(std::slice::from_mut(c), &project.audio_setup);
            }
            let layer_id = layer.layer_id.clone();
            ui.inspector
                .configure_gen_params(gen_config.as_ref(), Some(layer_id));
        } else {
            ui.inspector.configure_layer_effects(&[], None);
            ui.inspector.configure_gen_params(None, None);
            ui.inspector
                .audio_trigger_section_mut()
                .configure(None, &manifold_ui::panels::audio_trigger_section::AudioTriggerSectionConfig::default());
        }
    } else {
        ui.inspector.configure_layer_effects(&[], None);
        ui.inspector.configure_gen_params(None, None);
        ui.inspector
            .audio_trigger_section_mut()
            .configure(None, &manifold_ui::panels::audio_trigger_section::AudioTriggerSectionConfig::default());
    }

    // Clip chrome → inspector (per-clip effects removed)
    if let Some(clip_id) = &selection.primary_selected_clip_id {
        let clip = project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .find(|c| c.id == *clip_id);
        if let Some(clip) = clip {
            // Sync clip chrome MODE before build so the tree layout is correct.
            // Value sync (name, bpm, etc.) happens in push_state after build.
            let is_video = !clip.video_clip_id.is_empty();
            let is_gen = clip.generator_type != PresetTypeId::NONE;
            let is_audio = clip.is_audio();
            ui.inspector
                .clip_chrome_mut()
                .set_mode(true, is_video, is_gen, is_audio, clip.is_looping);
            // Feed the detection rows before build so the row count drives layout.
            if is_audio {
                use manifold_core::audio_clip_detection::{
                    quantize_grid_label, DetectionConfig,
                };
                use manifold_core::types::LayerType;
                use manifold_ui::panels::clip_chrome::{DetectInstrumentRow, DetectionView};

                let default_cfg;
                let (cfg, detection) = match clip.audio_detection.as_ref() {
                    Some(d) => (&d.config, Some(d)),
                    None => {
                        default_cfg = DetectionConfig::default();
                        (&default_cfg, None)
                    }
                };

                // Candidate routing layers (non-group) for the per-row dropdowns.
                let candidates: Vec<(manifold_core::LayerId, String)> = project
                    .timeline
                    .layers
                    .iter()
                    .filter(|l| l.layer_type != LayerType::Group)
                    .map(|l| (l.layer_id.clone(), l.name.clone()))
                    .collect();

                let instruments = cfg
                    .instruments
                    .iter()
                    .map(|inst| {
                        let count =
                            detection.map_or(0, |d| d.count(inst.trigger_type));
                        let layer_label = inst
                            .target_layer
                            .as_ref()
                            .and_then(|id| {
                                candidates.iter().find(|(lid, _)| lid == id).map(|(_, n)| n.clone())
                            })
                            .unwrap_or_else(|| "Auto".to_string());
                        DetectInstrumentRow {
                            label: format!("{:?}", inst.trigger_type),
                            enabled: inst.enabled,
                            sensitivity: inst.sensitivity,
                            count,
                            layer_label,
                        }
                    })
                    .collect();

                let view = DetectionView {
                    quantize_label: quantize_grid_label(cfg.quantize_on, cfg.quantize_step_beats),
                    onset_ms: (cfg.onset_compensation.0 * 1000.0) as f32,
                    has_analysis: detection.is_some_and(|d| d.analysis.is_some()),
                    instruments,
                };
                ui.inspector.clip_chrome_mut().set_detection(&view);
                ui.set_clip_detect_layers(candidates);
            }
        } else {
            ui.inspector
                .clip_chrome_mut()
                .set_mode(false, false, false, false, false);
        }
    } else {
        ui.inspector
            .clip_chrome_mut()
            .set_mode(false, false, false, false, false);
    }
}

/// Convert a slice of `PresetInstance` into [`ParamSurface`]s for the UI.
/// Unity: EffectCardState.SyncFromDataModel — populates all data-derived visual state.
///
/// Iterates BOTH the def-declared static block AND the per-instance
/// user-tail bindings, producing one [`ParamRow`] per slot in
/// `effect.param_values` order. The card renders a slider for every
/// exposed entry; hidden static slots and unchecked user-tail entries
/// (the latter are removed from `user_param_bindings` rather than
/// hidden, so they never reach this loop) are filtered at build time.
/// Build the per-row driver + envelope + automation modulation facts for one
/// preset instance's card, one [`RowMod`] per row (D3), all sized to `n` (the
/// card's param count). Shared by the effect and generator card builders —
/// the only thing that differs between them is `resolve`, the `param_id → slot
/// index` mapping (an effect resolves via `param_id_to_value_index`, a generator
/// via its graph/registry `row_index_of`). The rows are identical; the
/// per-card `has_drv` / `has_env` summary flags stay with each caller (the
/// generator card intentionally forces them false).
///
/// `resolve` maps a modulation row's `param_id` to its card slot index.
/// `latched` is `ContentState::automation_latched_params` — checked against
/// `(inst.id, lane.param_id)` for the overridden-gray state (P4 §7's dot).
/// Always `inst.id`, which (fixed 2026-07-11) is now also the card's own
/// DISPLAYED `effect_id` for both kinds — `preset_to_config` used to blank
/// the generator arm's `effect_id` to `EffectId::new("")`, so this function
/// and the card disagreed about a generator's identity even though both
/// ultimately read the same real, freshly-synthesized `EffectId` (see
/// `manifold_playback::automation`'s `AutomationLatches` doc comment).
pub(crate) fn build_card_modulation(
    inst: &PresetInstance,
    n: usize,
    resolve: impl Fn(&str) -> Option<usize>,
    latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> Vec<RowMod> {
    let mut rows = vec![RowMod::default(); n];
    if let Some(ref drivers) = inst.drivers {
        for d in drivers {
            if !d.enabled {
                continue;
            }
            let Some(pi) = resolve(d.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.driver_active = true;
            row.trim_min = d.trim_min;
            row.trim_max = d.trim_max;
            row.driver_beat_div_idx = beat_div_to_button_index(d.beat_division.base_division());
            row.driver_waveform_idx = d.waveform as i32;
            row.driver_reversed = d.reversed;
            row.driver_dotted = d.beat_division.is_dotted();
            row.driver_triplet = d.beat_division.is_triplet();
            row.driver_free_period = d.free_period_beats;
        }
    }
    if let Some(ref envelopes) = inst.envelopes {
        for env in envelopes {
            if !env.enabled {
                continue;
            }
            let Some(pi) = resolve(env.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.envelope_active = true;
            row.target_norm = env.target_normalized;
            row.env_decay = env.decay_beats;
        }
    }
    if let Some(ref lanes) = inst.automation_lanes {
        for lane in lanes {
            // Enabled + non-empty only (§7: "an empty/disabled lane shows no
            // dot") — matches the sampler's own `has_lanes` gate in
            // `manifold_playback::automation`.
            if !lane.enabled || lane.points.is_empty() {
                continue;
            }
            let Some(pi) = resolve(lane.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.automation_active = true;
            row.automation_overridden = latched
                .iter()
                .any(|(eid, pid)| *eid == inst.id && *pid == lane.param_id);
        }
    }
    rows
}

/// Build the per-param audio-modulation display state for a card from the
/// instance's `audio_mods`. The card-level send list (`send_labels`/`send_ids`)
/// is filled separately by [`attach_audio_sends`] (it needs the project's
/// `AudioSetup`, which this per-instance builder doesn't carry).
///
/// §9: a trigger-gate row's config is a normal `ParameterAudioMod` like any
/// other, so this single walk covers it too — `trigger_mode_idx` is read off
/// `am.trigger_mode` (defaulting to `Both`, mirroring the evaluator's
/// `unwrap_or(TriggerFireMode::Both)` fallback) alongside the other fields.
/// No `is_trigger_gate` awareness needed here; only the UI's collapsed-row
/// badge and Mode row care which row it is.
pub(crate) fn build_audio_card_state(
    inst: &PresetInstance,
    n: usize,
    resolve: impl Fn(&str) -> Option<usize>,
) -> AudioCardState {
    let mut a = AudioCardState {
        rows: vec![AudioRowState::default(); n],
        send_labels: Vec::new(),
        send_ids: Vec::new(),
    };
    for am in inst.audio_mods.iter().flatten() {
        if !am.enabled {
            continue;
        }
        let Some(pi) = resolve(am.param_id.as_ref()).filter(|&pi| pi < n) else {
            continue;
        };
        let row = &mut a.rows[pi];
        row.active = true;
        row.send_id = Some(am.source.send_id.clone());
        row.range_min = am.shape.range_min;
        row.range_max = am.shape.range_max;
        row.invert = am.shape.invert;
        row.rate = am.shape.rate_of_change;
        row.sensitivity = am.shape.sensitivity;
        row.attack_ms = am.shape.attack_ms;
        row.release_ms = am.shape.release_ms;
        row.kind_idx = am.source.feature.kind.index() as i32;
        row.band_idx = am.source.feature.band.index() as i32;
        // PARAM_STEP_ACTIONS D3: an unset `trigger_mode`'s effective default
        // depends on the mod's action — a gate's (or a plain Continuous mod's)
        // arm-time default is `Both` (adding audio must not silently kill clip
        // launches, §9 U3); a Step/Random mod's default is `Transient` (a step
        // mod with no audio intent armed is meaningless — the user opened an
        // audio drawer). This must track the evaluator's own default exactly,
        // or the drawer shows a Mode selection that isn't what actually fires.
        let default_mode = if matches!(am.action, manifold_core::audio_mod::TriggerAction::Continuous)
        {
            manifold_core::audio_trigger::TriggerFireMode::Both
        } else {
            manifold_core::audio_trigger::TriggerFireMode::Transient
        };
        row.trigger_mode_idx = match am.trigger_mode.unwrap_or(default_mode) {
            manifold_core::audio_trigger::TriggerFireMode::ClipEdge => 0,
            manifold_core::audio_trigger::TriggerFireMode::Transient => 1,
            manifold_core::audio_trigger::TriggerFireMode::Both => 2,
        };
        match am.action {
            manifold_core::audio_mod::TriggerAction::Continuous => {
                row.action_idx = 0;
            }
            manifold_core::audio_mod::TriggerAction::Step { amount, wrap } => {
                row.action_idx = 1;
                row.step_amount = amount;
                row.wrap_idx = match wrap {
                    manifold_core::audio_mod::WrapMode::Wrap => 0,
                    manifold_core::audio_mod::WrapMode::Bounce => 1,
                    manifold_core::audio_mod::WrapMode::Clamp => 2,
                };
            }
            manifold_core::audio_mod::TriggerAction::Random => {
                row.action_idx = 2;
            }
        }
    }
    a
}

/// Reusable driver/envelope/audio-mod lookup for a SINGLE param id on a
/// [`PresetInstance`] — the same authority chain [`preset_to_config`] walks
/// for every card row ([`build_card_modulation`] + [`build_audio_card_state`]),
/// scoped down from a whole card's row list to one id. SCENE_PANEL_UX_DESIGN.md's
/// UX-P3b sizing amendment names this refactor as its own deliverable: the
/// Scene Setup panel's exposed-param rows resolve their driver/envelope/
/// audio-mod facts through this, instead of re-deriving the lookup a second
/// time against the layer's generator `PresetInstance`.
///
/// Returns `(Vec<RowMod>, AudioCardState)` sized to `n = 1` — index `0` is
/// always the queried param, regardless of its real position in `inst.params`.
/// `automation_latched` is `ContentState::automation_latched_params`, same as
/// every other caller of `build_card_modulation`.
///
/// Un-suppression trigger fired (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md
/// C-P1a): called by [`row_modulation_for_id`] below, which flattens this
/// query's sized-to-1 output into one [`RowModulation`] scalar struct per
/// Environment/Fog row for `sync_inspector_data`'s scene section.
pub(crate) fn lookup_param_mod_for_id(
    inst: &PresetInstance,
    param_id: &str,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> (Vec<RowMod>, AudioCardState) {
    let resolve = |id: &str| (id == param_id).then_some(0);
    (
        build_card_modulation(inst, 1, resolve, automation_latched),
        build_audio_card_state(inst, 1, resolve),
    )
}

/// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a (D3): flatten
/// [`lookup_param_mod_for_id`]'s sized-to-1 `(Vec<RowMod>, AudioCardState)`
/// into one scalar [`manifold_ui::panels::scene_setup_panel::RowModulation`]
/// for a single Environment/Fog row. `inst = None` (no generator on the
/// layer yet, or the layer isn't a generator) returns the idle default —
/// same "no modulation, not an error" contract `lookup_param_mod_for_id`
/// itself has for an un-modulated param.
/// BUG-249: the scene-row entry point — translate `(node_doc_id, param_key)`
/// to the REAL exposed-param binding id before the modulation lookup. Scene
/// rows used to query by their synthesized `scene.{doc}.{param}` id, which
/// never exists on `inst.params`, so the UI read back the very arm it had
/// stored against an id the runtime silently drops (the closed loop the bug
/// names). An unexposed param has no binding → idle default, same "no
/// modulation, not an error" contract as `inst = None`.
pub(crate) fn scene_row_modulation(
    inst: Option<&PresetInstance>,
    effective_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    node_doc_id: u32,
    param_key: &str,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> manifold_ui::panels::scene_setup_panel::RowModulation {
    // Instance graph first; a TRACKING instance (graph: None — fresh
    // imports) resolves against the effective catalog def instead.
    let real_id = inst
        .and_then(|i| i.binding_id_for_node_param(node_doc_id, param_key))
        .or_else(|| {
            manifold_core::effects::binding_id_for_node_param_in(
                effective_def?,
                node_doc_id,
                param_key,
            )
        });
    match real_id {
        Some(id) => row_modulation_for_id(inst, &id, automation_latched),
        None => manifold_ui::panels::scene_setup_panel::RowModulation::default(),
    }
}

pub(crate) fn row_modulation_for_id(
    inst: Option<&PresetInstance>,
    param_id: &str,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> manifold_ui::panels::scene_setup_panel::RowModulation {
    use manifold_ui::panels::scene_setup_panel::RowModulation;
    let Some(inst) = inst else {
        return RowModulation::default();
    };
    let (m, a) = lookup_param_mod_for_id(inst, param_id, automation_latched);
    let row = &m[0];
    let audio_row = &a.rows[0];
    RowModulation {
        driver_active: row.driver_active,
        trim_min: row.trim_min,
        trim_max: row.trim_max,
        driver_beat_div_idx: row.driver_beat_div_idx,
        driver_waveform_idx: row.driver_waveform_idx,
        driver_reversed: row.driver_reversed,
        driver_dotted: row.driver_dotted,
        driver_triplet: row.driver_triplet,
        driver_free_period: row.driver_free_period,
        envelope_active: row.envelope_active,
        target_norm: row.target_norm,
        env_decay: row.env_decay,
        automation_active: row.automation_active,
        automation_overridden: row.automation_overridden,
        audio_active: audio_row.active,
        audio_send_id: audio_row.send_id.clone(),
        audio_kind_idx: audio_row.kind_idx,
        audio_band_idx: audio_row.band_idx,
        audio_range_min: audio_row.range_min,
        audio_range_max: audio_row.range_max,
        audio_invert: audio_row.invert,
        audio_rate: audio_row.rate,
        audio_sensitivity: audio_row.sensitivity,
        audio_attack_ms: audio_row.attack_ms,
        audio_release_ms: audio_row.release_ms,
        audio_trigger_mode_idx: audio_row.trigger_mode_idx,
        audio_action_idx: audio_row.action_idx,
        audio_step_amount: audio_row.step_amount,
        audio_wrap_idx: audio_row.wrap_idx,
    }
}

#[cfg(test)]
mod param_mod_lookup_tests {
    use super::*;
    use manifold_core::effects::ParameterDriver;
    use manifold_core::types::{BeatDivision, DriverWaveform};

    fn driver_for(param_id: &str) -> ParameterDriver {
        ParameterDriver {
            param_id: std::borrow::Cow::Owned(param_id.to_string()),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.1,
            trim_max: 0.9,
            reversed: false,
            free_period_beats: None,
            legacy_param_index: None,
            is_paused_by_user: false,
        }
    }

    /// UX-P3b (SCENE_PANEL_UX_DESIGN.md sizing amendment): the reusable
    /// single-param query must find the SAME driver `preset_to_config`'s
    /// `build_card_modulation` would find for that id at its real card
    /// position — it just doesn't need that position, because it always
    /// reports at index 0.
    #[test]
    fn lookup_finds_the_named_params_driver_regardless_of_manifest_position() {
        let mut inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        inst.drivers = Some(vec![driver_for("intensity")]);

        let (modulation, _audio) = lookup_param_mod_for_id(&inst, "intensity", &[]);
        assert!(modulation[0].driver_active);
        assert_eq!(modulation[0].trim_min, 0.1);
        assert_eq!(modulation[0].trim_max, 0.9);
    }

    /// A driver on a DIFFERENT param id must not leak into this param's slot
    /// — the query is scoped to the exact id it was asked about, not "any
    /// driver on the instance."
    #[test]
    fn lookup_ignores_drivers_on_other_param_ids() {
        let mut inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        inst.drivers = Some(vec![driver_for("fill")]);

        let (modulation, audio) = lookup_param_mod_for_id(&inst, "intensity", &[]);
        assert!(!modulation[0].driver_active);
        assert!(!audio.rows[0].active);
    }

    /// No drivers/envelopes/audio-mods at all → an idle single-slot result,
    /// not a panic (the scene panel calls this for every exposed row on
    /// every rebuild, including ones with no modulation yet).
    #[test]
    fn lookup_on_unmodulated_param_returns_idle_slot() {
        let inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        let (modulation, audio) = lookup_param_mod_for_id(&inst, "intensity", &[]);
        assert!(!modulation[0].driver_active);
        assert!(!modulation[0].envelope_active);
        assert!(!audio.rows[0].active);
    }
}

/// Resolve a send's routed channels to a human label for the Audio Setup row:
/// the channel name(s) joined with " + ", or "Not routed" when empty. Falls
/// back to a 1-based index when no device metadata is available.
fn channel_label(
    device: Option<&manifold_audio::directory::DeviceInfo>,
    is_tap: bool,
    channels: &[u16],
) -> String {
    if channels.is_empty() {
        return "Not routed".to_string();
    }
    let name_of = |ch: u16| -> String {
        // A tap is a fixed stereo mixdown — channel 0/1 are Left/Right, matching
        // the tap channel picker. A hardware device uses its platform names.
        if is_tap {
            return match ch {
                0 => "Left".to_string(),
                1 => "Right".to_string(),
                n => format!("Channel {}", n + 1),
            };
        }
        device
            .and_then(|d| d.channels.get(ch as usize))
            .map(|c| c.display_name())
            .unwrap_or_else(|| format!("Channel {}", ch + 1))
    };
    channels.iter().map(|&ch| name_of(ch)).collect::<Vec<_>>().join(" + ")
}

/// Map a base BeatDivision to its button index (0-10).
/// Reverse of BeatDivision::from_button_index.
fn beat_div_to_button_index(div: BeatDivision) -> i32 {
    match div {
        BeatDivision::ThirtySecond => 0,
        BeatDivision::Sixteenth => 1,
        BeatDivision::Eighth | BeatDivision::EighthDotted | BeatDivision::EighthTriplet => 2,
        BeatDivision::Quarter | BeatDivision::QuarterDotted | BeatDivision::QuarterTriplet => 3,
        BeatDivision::Half | BeatDivision::HalfDotted | BeatDivision::HalfTriplet => 4,
        BeatDivision::Whole | BeatDivision::WholeDotted | BeatDivision::WholeTriplet => 5,
        BeatDivision::TwoWhole | BeatDivision::TwoWholeDotted => 6,
        BeatDivision::FourWhole => 7,
        BeatDivision::EightWhole => 8,
        BeatDivision::SixteenWhole => 9,
        BeatDivision::ThirtyTwoWhole => 10,
    }
}

#[cfg(test)]
mod build_audio_card_state_trigger_mode_tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::audio_trigger::TriggerFireMode;
    use manifold_core::effects::PresetInstance;
    use manifold_core::id::AudioSendId;

    fn resolve<'a>(params: &'a [&'a str]) -> impl Fn(&str) -> Option<usize> + 'a {
        move |id| params.iter().position(|&p| p == id)
    }

    /// §9: a trigger-gate row's fire mode lives on the mod itself
    /// (`ParameterAudioMod.trigger_mode`), not a separate per-instance
    /// config — `build_audio_card_state` reads it into `trigger_mode_idx`
    /// in the SAME walk that populates `active`/`send_id`/`band_idx`/etc.
    /// This is the function `param_surface` calls to populate
    /// `ParamSurface.audio`, so a green test here is the proof the
    /// config the card sees actually carries the project's live mode, not
    /// just that the model round-trips in isolation.
    #[test]
    fn trigger_mode_reads_off_the_mod_alongside_every_other_field() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        );
        m.trigger_mode = Some(TriggerFireMode::Both);
        inst.audio_mods = Some(vec![m]);

        let params = ["amount", "clip_trigger"];
        let cfg = build_audio_card_state(&inst, params.len(), resolve(&params));

        assert!(!cfg.rows[0].active);
        assert!(cfg.rows[1].active);
        assert_eq!(cfg.rows[0].send_id, None);
        assert_eq!(cfg.rows[1].send_id, Some(AudioSendId::new("send-kick")));
        assert_eq!(cfg.rows[1].band_idx, AudioBand::Low.index() as i32);
        assert_eq!(cfg.rows[1].trigger_mode_idx, 2); // Both
    }

    /// A disabled mod (armed once, then disarmed via the "A" button, which
    /// flips `enabled` without clearing the rest) reads as fully inactive —
    /// the standard per-param "skip if !enabled" rule covers a trigger-gate
    /// row automatically now; no trigger-specific gate to keep in sync.
    #[test]
    fn disabled_mod_reads_as_inactive() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.enabled = false;
        m.trigger_mode = Some(TriggerFireMode::ClipEdge);
        inst.audio_mods = Some(vec![m]);

        let params = ["clip_trigger"];
        let cfg = build_audio_card_state(&inst, params.len(), resolve(&params));
        assert!(!cfg.rows[0].active);
    }

    /// No `trigger_mode` set on the mod (defensive — §9 U3 always arms with
    /// `Some(Both)`, but a hand-built or pre-§9-migrated mod could carry
    /// `None`) reads the SAME `Both` fallback the evaluator uses
    /// (`unwrap_or(TriggerFireMode::Both)`), so the badge/drawer never
    /// disagrees with what actually fires.
    #[test]
    fn missing_trigger_mode_defaults_to_both() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        assert_eq!(m.trigger_mode, None);
        inst.audio_mods = Some(vec![m]);

        let params = ["clip_trigger"];
        let cfg = build_audio_card_state(&inst, params.len(), resolve(&params));
        assert_eq!(cfg.rows[0].trigger_mode_idx, 2); // Both
    }
}

/// Display name for one D6 modifier-stack atom (P2 shows the chain as a
/// display-only line; the interactive stack is P5). Falls back to the raw
/// type_id for anything outside the curated D6 list — never blank, per D3's
/// "custom" degrade rule.
fn modifier_display_name(type_id: &str) -> String {
    match type_id {
        "node.bend_mesh" => "Bend".to_string(),
        "node.twist_mesh" => "Twist".to_string(),
        "node.taper_mesh" => "Taper".to_string(),
        "node.push_along_normals" => "Inflate".to_string(),
        "node.push_mesh" => "Displace by Texture".to_string(),
        "node.morph_mesh" => "Morph".to_string(),
        "node.rotate_3d" => "Rotate".to_string(),
        other => other.to_string(),
    }
}

/// End-to-end round-trip: every fire-meter key the UI's `update_fire_meters`
/// requests must resolve in the SAME `FireMeterCapture` the content-thread
/// evaluators (`evaluate_all_audio_mods` + `LiveTriggerState::evaluate`)
/// produce for the SAME `Project`. Producer (`manifold-playback`) and
/// consumer (`ParamCardPanel`/`AudioTriggerSection` via this module's
/// `sync_inspector_data`) each independently recompute a fire-meter key from
/// an instance's identity — nothing previously verified they agree. That's
/// exactly the bug class the 2026-07-11 generator-card fix closed:
/// `preset_to_config`'s Generator arm used to display `EffectId::new("")`
/// instead of the real `inst.id`, so a generator's audio-mod meter asked for
/// a key the content thread never pushed anything under.
///
/// Builds ONE project carrying all four fire-meter-hosting shapes on a
/// single layer (so one `sync_inspector_data` + `build_in_rect` pass reaches
/// every one of them — `sync_inspector_data` only configures the ACTIVE
/// layer's effects/gen-params/clip-triggers, so spreading the shapes across
/// separate layers like `ui_snapshot::fixtures::inspector_scene` does would
/// mean only one shape's layer is ever active at a time):
///   (a) a GENERATOR instance's `is_trigger_gate` audio mod (Plasma's
///       `clip_trigger`) — the exact shape the generator-card bug shipped
///       under;
///   (b) an EFFECT's plain continuous audio mod on a non-gate param (Bloom)
///       — the shape the 2026-07-11 widening (128→512 `MAX_FIRE_METERS`,
///       `fire_meters.push` moved above the gate-mode fork) started
///       metering;
///   (c) an EFFECT's `is_trigger_gate` audio mod (Strobe's `clip_trigger`);
///   (d) a `Layer.clip_triggers` row.
/// Bloom/Strobe/Plasma are real shipping presets, chosen to mirror
/// `ui_snapshot::fixtures::inspector_scene` (the BUG-082/P3c gate scene) —
/// folded onto one layer instead of three so a single active-layer build
/// surfaces all four.
#[cfg(test)]
mod fire_meter_roundtrip_tests {
    use super::*;
    use manifold_core::audio_mod::{
        AudioBand, AudioFeature, AudioFeatureKind, AudioModSource, ParameterAudioMod,
    };
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::{
        FireMeterCapture, LayerClipTrigger, TriggerFireMode, fire_meter_key_for_clip_trigger,
        fire_meter_key_for_param,
    };
    use manifold_core::layer::Layer;
    use manifold_core::project::Project;
    use manifold_core::{EffectId, LayerId, PresetTypeId, Seconds};
    use manifold_playback::live_trigger::LiveTriggerState;
    use manifold_playback::modulation::{TriggerPulse, evaluate_all_audio_mods};
    use manifold_ui::node::Rect;

    /// Zero attack/release so a mod's conditioned level snaps instantly to
    /// its raw input within ONE evaluation tick — the same pattern
    /// `manifold-playback::modulation`'s own tests use for deterministic
    /// single-tick assertions (`attach_full_range_low_mod`).
    fn instant_mod(
        param_id: &str,
        send_id: &manifold_core::AudioSendId,
        feature: AudioFeature,
    ) -> ParameterAudioMod {
        let mut m = ParameterAudioMod::new(param_id.to_string().into(), send_id.clone(), feature);
        m.shape.attack_ms = 0.0;
        m.shape.release_ms = 0.0;
        m
    }

    /// The fixture project plus the identity facts the test needs to compute
    /// each shape's expected fire-meter key.
    struct Fixture {
        project: Project,
        layer_id: LayerId,
        bloom_id: EffectId,
        bloom_param: String,
        strobe_id: EffectId,
        plasma_id: EffectId,
    }

    /// One layer carrying all four fire-meter-hosting shapes at once: a
    /// generator (Plasma) with a gate mod, an effects chain (Bloom
    /// continuous + Strobe gate) riding on top of it, and its own
    /// clip-trigger row. A generator layer carrying an effects chain is the
    /// normal MANIFOLD shape (post-processing over a procedural source), so
    /// this isn't a contrived overlap — it's just all on one layer instead
    /// of `inspector_scene`'s three, specifically so ONE `active_layer`
    /// build reaches all four.
    fn build_fixture() -> Fixture {
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();

        let mut layer = Layer::new_generator("PLASMA".into(), PresetTypeId::PLASMA, 0);
        let layer_id = layer.layer_id.clone();

        // (a) GENERATOR instance, `is_trigger_gate` mod on `clip_trigger`.
        let mut gate_on_gen = instant_mod(
            "clip_trigger",
            &send_id,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        gate_on_gen.trigger_mode = Some(TriggerFireMode::Both);
        layer.gen_params_or_init().audio_mods = Some(vec![gate_on_gen]);
        let plasma_id = layer.gen_params().unwrap().id.clone();

        // (b) EFFECT, plain continuous mod on a non-gate param — the
        // newly-metered shape.
        let mut bloom = PresetInstance::new(PresetTypeId::BLOOM);
        bloom.init_defaults();
        let bloom_param = manifold_core::preset_definition_registry::try_get(bloom.effect_type())
            .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
            .expect("Bloom has at least one param");
        bloom.audio_mods = Some(vec![instant_mod(
            &bloom_param,
            &send_id,
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
        )]);
        let bloom_id = bloom.id.clone();

        // (c) EFFECT, `is_trigger_gate` mod on `clip_trigger`.
        let mut strobe = PresetInstance::new(PresetTypeId::new("Strobe"));
        strobe.init_defaults();
        let mut gate_on_fx = instant_mod(
            "clip_trigger",
            &send_id,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        );
        gate_on_fx.trigger_mode = Some(TriggerFireMode::Transient);
        strobe.audio_mods = Some(vec![gate_on_fx]);
        let strobe_id = strobe.id.clone();

        layer.effects = Some(vec![bloom, strobe]);

        // (d) `Layer.clip_triggers` row.
        let mut trigger = LayerClipTrigger::new(AudioModSource {
            send_id: send_id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Kick, AudioBand::Low),
        });
        trigger.enabled = true;
        trigger.shape.attack_ms = 0.0;
        trigger.shape.release_ms = 0.0;
        layer.clip_triggers.push(trigger);

        let mut project = Project::default();
        project.audio_setup.sends.push(send);
        project.timeline.layers.push(layer);

        Fixture { project, layer_id, bloom_id, bloom_param, strobe_id, plasma_id }
    }

    /// One send ("Kick") with a nonzero level on every band/feature the
    /// fixture's four mods read, so a real evaluation tick leaves every one
    /// of them in the producer's `FireMeterCapture`.
    fn hot_snapshot() -> manifold_core::audio_features::AudioFeatureSnapshot {
        let mut bands = [manifold_core::BandFeatures::default(); 4];
        bands[AudioBand::Full.index()].amplitude = 0.8; // (b) Bloom
        bands[AudioBand::Full.index()].transients = 0.8; // (a) Plasma gate
        bands[AudioBand::Low.index()].transients = 0.8; // (c) Strobe gate
        bands[AudioBand::Low.index()].kick = 0.8; // (d) clip-trigger row (Kick always reads Low)
        manifold_core::audio_features::AudioFeatureSnapshot {
            sends: vec![manifold_core::SendFeatures { bands, ..Default::default() }],
        }
    }

    #[test]
    fn every_ui_requested_fire_meter_key_resolves_in_the_producer_capture() {
        let Fixture { mut project, layer_id, bloom_id, bloom_param, strobe_id, plasma_id } =
            build_fixture();

        // ── Producer: the SAME two evaluators the content thread runs every
        // tick (`manifold_playback::modulation::evaluate_all_audio_mods` for
        // every enabled param audio mod, `LiveTriggerState::evaluate` for
        // clip triggers) — see `crates/manifold-playback/tests/engine_tick.rs`
        // and `live_trigger.rs`'s own tests for the same two-call pattern.
        let snapshot = hot_snapshot();
        let dt = Seconds(1.0 / 60.0);
        let mut fire_meters = FireMeterCapture::default();
        let mut pulses: Vec<TriggerPulse> = Vec::new();
        evaluate_all_audio_mods(&mut project, &snapshot, dt, &mut pulses, &[], &mut fire_meters);
        let mut live_trigger = LiveTriggerState::default();
        live_trigger.evaluate(&snapshot, &project.audio_setup, &project.timeline.layers, dt, &mut fire_meters);

        // ── Consumer: the real UI build — `sync_inspector_data` (the same
        // function `ui_snapshot::render_ui_scene` calls) configures the
        // inspector's cards + AUDIO TRIGGERS section from this SAME project,
        // then `build_in_rect` (what the graph-editor window's inspector
        // column, and the main window via `UIRoot::build`, both call)
        // materializes them into a real `UITree`.
        let mut ui = UIRoot::new();
        let mut selection = SelectionState::default();
        selection.select_layer(layer_id.clone());
        sync_inspector_data(&mut ui, &project, Some(0), &selection, &[]);

        // Open every fixture drawer. (a)/(b)/(c) need no explicit "open": an
        // armed audio mod's drawer builds automatically — a toggle/trigger
        // row's drawer whenever its mod is enabled (`build_toggle_trigger_row`),
        // a slider row's whenever Audio is its only (or resolved) active
        // mod-tab (`build_param_row`), both in `param_slider_shared.rs`. Only
        // the AUDIO TRIGGERS section's clip-trigger row (d) is gated behind
        // its OWN collapse/expand UI state (`AudioTriggerSection`), which
        // `configure()` never touches — so it must be opened explicitly here.
        ui.inspector.audio_trigger_section_mut().toggle_collapsed();
        ui.inspector.audio_trigger_section_mut().toggle_row_expanded(0);

        ui.build_inspector_in_rect(Rect::new(0.0, 0.0, 640.0, 4000.0));

        // Record every key the UI requests while pushing levels. `fire_level`
        // is `&dyn Fn`, not `FnMut`, so the recorder needs interior
        // mutability.
        let requested = std::cell::RefCell::new(Vec::<u64>::new());
        let record = |key: u64| -> Option<f32> {
            requested.borrow_mut().push(key);
            fire_meters.get(key)
        };
        ui.inspector.update_fire_meters(&mut ui.tree, &record, dt.0 as f32);
        let requested = requested.into_inner();

        // The four keys the PRODUCER computed for the exact same instances —
        // the ground truth the UI's own keys must agree with.
        let expected: Vec<(&str, u64)> = vec![
            (
                "(a) Plasma generator gate mod (clip_trigger)",
                fire_meter_key_for_param(plasma_id.as_str(), "clip_trigger"),
            ),
            (
                "(b) Bloom continuous mod",
                fire_meter_key_for_param(bloom_id.as_str(), &bloom_param),
            ),
            (
                "(c) Strobe gate mod (clip_trigger)",
                fire_meter_key_for_param(strobe_id.as_str(), "clip_trigger"),
            ),
            (
                "(d) layer clip-trigger row 0",
                fire_meter_key_for_clip_trigger(layer_id.as_str(), 0),
            ),
        ];

        let requested_set: std::collections::HashSet<u64> = requested.iter().copied().collect();

        // (i) every fixture drawer we opened must have had ITS EXACT expected
        // key requested — catches three distinct failure shapes in one
        // check: "meter built but never updated" and "drawer missing its
        // meter" (an audio_configs slot that resolved but whose `DrawerIds`
        // carries no Meter widget, so `update_fire_meters` silently skips
        // it) both mean the key is simply absent from `requested_set`; a
        // THIRD shape — the UI's card computed a DIFFERENT identity for the
        // same instance than the producer did (a blanked/stale `EffectId`,
        // e.g. the pre-2026-07-11 generator-card bug this test targets) —
        // also fails here: the row's drawer still opens and still requests
        // *some* key, just not this one, so `expected`'s real key is still
        // absent from `requested_set`.
        for (label, key) in &expected {
            assert!(
                requested_set.contains(key),
                "the UI never requested the expected fire-meter key for {label}. Either its \
                 drawer never built (the audio_configs slot stayed None), its DrawerIds carries \
                 no Meter widget (update_fire_meters silently skips both), OR — the producer/\
                 consumer divergence class this test exists to catch — the card computed a \
                 DIFFERENT identity key for this same instance than the content thread did (a \
                 blanked or stale EffectId/LayerId), so it requested some other key instead"
            );
        }
        assert_eq!(
            requested_set.len(),
            expected.len(),
            "expected exactly {} open fire-meter rows ({:?}) but the UI requested {} distinct \
             keys — an extra drawer built somewhere this fixture didn't arm",
            expected.len(),
            expected.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            requested_set.len(),
        );

        // (ii) every key the UI requested must resolve against the SAME
        // `FireMeterCapture` the content thread produced — the round-trip
        // proof, and the exact bug class the 2026-07-11 generator-card fix
        // closed: `preset_to_config`'s Generator arm once displayed
        // `EffectId::new("")` instead of the real `inst.id`, so a generator
        // card's audio-mod meter computed a key the content thread never
        // pushed anything under.
        for (label, key) in &expected {
            assert!(
                fire_meters.get(*key).is_some(),
                "producer/consumer key divergence for {label}: the UI's fire-meter key {key} \
                 was never recorded by the content-thread FireMeterCapture — the UI and the \
                 evaluators disagree about this instance's identity"
            );
        }
    }
}
