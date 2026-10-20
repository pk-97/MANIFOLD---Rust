//! Card-surface projection: THE `param_surface` manifest walk that builds
//! effect/generator `ParamSurface`s, its thin adapters and helpers, the
//! per-frame card VALUE sync, and the macro-mapping label. Moved from
//! state_sync.rs (P-P, UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_ui::panels::param_card::{
    ParamCardKind, ParamCardStringChoice, ParamCardStringInfo, RowMod,
};
use manifold_ui::panels::param_slider_shared::{AbletonMappingDisplay, AudioSendChoice};
use manifold_ui::param_surface::{ParamRow, ParamSurface, RowMapping, RowSpec, RowValue};
use crate::ui_root::UIRoot;

use super::inspector::{audio_row_state, build_card_modulation};

/// OSC address scope for effect param configs.
/// Master effects use `/master/`, layer effects use `/layer/{id}/`, clips have no OSC.
#[derive(Clone, Copy)]
pub(crate) enum OscScope<'a> {
    Master,
    Layer(&'a str),
}

/// Which manifest params [`param_surface`] turns into rows. No default — every
/// call states its intent, because the two surfaces this projection feeds want
/// opposite things (BUG-313's regression was a single hard-coded filter serving
/// both).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceVisibility {
    /// The curated outer effect/generator CARD: hide any `card_visible: false`
    /// param. It stays a fully addressable manifest entry (OSC, Ableton,
    /// macros, drivers all still resolve it by id) — it just never becomes a
    /// card row.
    CuratedCard,
    /// Every manifest param becomes a row. The Scene tab's full generator
    /// surface uses this so scale/material rows reach the panel; the panel
    /// then filters by item ownership, not by `card_visible`.
    All,
}

/// Push per-frame card VALUES (slider fill + readout, enabled toggle, card
/// name) from `project` into the already-configured inspector cards of any
/// window's `ui` — master effects, active layer's effects, generator params.
/// Window-agnostic: `push_state` calls it for the main window every frame,
/// and the graph-editor window's present path calls it on its own
/// `ed.ui_root` with the same `local_project`/`active_layer`, so card sliders
/// track drivers / mappings / envelopes in both windows instead of freezing
/// between structural syncs. No drag guard needed here: the actively-dragged
/// field is restored into `local_project` upstream of every call
/// (`app_render.rs`'s snapshot-drain `drag.apply`), so this writes the user's
/// own value straight back — user-owned in both windows.
pub fn sync_card_values(ui: &mut UIRoot, project: &Project, active_layer: Option<usize>) {
    let tree = &mut ui.tree;
    // Master effects
    for (i, effect) in project.settings.master_effects.iter().enumerate() {
        if let Some(card) = ui.inspector.master_effect_mut(i) {
            card.sync_effect_name(
                tree,
                manifold_core::preset_type_registry::display_name(effect.effect_type()),
            );
            card.sync_enabled(tree, effect.enabled);
            crate::ui_translate::with_param_slots(&effect.params, |slots| {
                card.sync_values(tree, slots)
            });
        }
    }

    // Layer effects
    if let Some(idx) = active_layer
        && let Some(layer) = project.timeline.layers.get(idx)
        && let Some(effects) = &layer.effects
    {
        for (i, effect) in effects.iter().enumerate() {
            if let Some(card) = ui.inspector.layer_effect_mut(i) {
                card.sync_effect_name(
                    tree,
                    manifold_core::preset_type_registry::display_name(effect.effect_type()),
                );
                card.sync_enabled(tree, effect.enabled);
                crate::ui_translate::with_param_slots(&effect.params, |slots| {
                    card.sync_values(tree, slots)
                });
            }
        }
    }

    // Generator params (stored on layer, not clip)
    if let Some(idx) = active_layer
        && let Some(layer) = project.timeline.layers.get(idx)
        && let Some(gp_state) = layer.gen_params()
        && let Some(gp) = ui.inspector.gen_params_mut()
    {
        gp.sync_gen_type_name(
            tree,
            manifold_core::preset_type_registry::display_name(gp_state.generator_type()),
        );
        crate::ui_translate::with_param_slots(&gp_state.params, |slots| {
            gp.sync_values(tree, slots)
        });
    }

    // Modifier cards (SCENE_MODIFIER_FRAMEWORK section 3.7): the modifier
    // rows ARE the layer's generator manifest rows — the same id-joined slot
    // stream the generator card syncs from (drivers/envelopes/mappings on
    // modifier rows update in place, no structural sync).
    if let Some(idx) = active_layer
        && let Some(layer) = project.timeline.layers.get(idx)
        && let Some(gp_state) = layer.gen_params()
    {
        if let Some(def) = gp_state.graph.as_ref() {
            for card in ui
                .inspector
                .modifier_cards_mut()
                .iter_mut()
            {
                crate::ui_translate::with_param_slots(&gp_state.params, |slots| {
                    card.sync_values(tree, slots)
                });
                if let Some(instance) = card.modifier_info().and_then(|info|
                    def.scene_modifiers.iter().find(|instance| instance.id == info.instance_id)) {
                    card.sync_enabled(tree, modifier_enabled_value(gp_state, def, instance));
                }
            }
        } else {
            for card in ui.inspector.modifier_cards_mut() {
                crate::ui_translate::with_param_slots(&gp_state.params, |slots| {
                    card.sync_values(tree, slots)
                });
            }
        }
    }
    if let Some((_, layer)) = ui.scene_setup_panel.live_layer_id()
        .and_then(|id| project.timeline.find_layer_by_id(id))
        && let Some(host) = layer.gen_params()
        && let Some(def) = host.graph.as_ref()
    {
        for card in ui.scene_setup_panel.force_cards_mut() {
            if let Some(instance) = card.modifier_info().and_then(|info|
                def.scene_modifiers.iter().find(|instance| instance.id == info.instance_id))
            {
                card.sync_enabled(tree, modifier_enabled_value(host, def, instance));
            }
        }
    }
}

/// Stamp the card-level available-send list (labels + ids) onto every card
/// config, from the project's `AudioSetup`. One pass after the configs are
/// built, so the per-instance builders stay project-agnostic.
pub(crate) fn attach_audio_sends(configs: &mut [ParamSurface], setup: &manifold_core::audio_setup::AudioSetup) {
    let sends = audio_send_choices(setup);
    for c in configs.iter_mut() {
        c.audio_sends = sends.clone();
        for sp in &mut c.string_params {
            if sp.key != "audioSend" || !sp.use_dropdown {
                continue;
            }
            let (choices, display) = audio_send_string_state(&sp.value, setup);
            sp.dropdown_choices = choices;
            sp.display_value = Some(display);
        }
    }
}

/// Attach every project-backed source choice at the structural projection seam.
/// Audio sends and clip-trigger assignments therefore share one cold/dirty pass.
pub(crate) fn attach_project_sources(configs: &mut [ParamSurface], project: &Project) {
    attach_audio_sends(configs, &project.audio_setup);
    super::trigger_routing::attach_trigger_sources(configs, project);
}

pub(crate) fn audio_send_choices(
    setup: &manifold_core::audio_setup::AudioSetup,
) -> Vec<AudioSendChoice> {
    setup
        .sends
        .iter()
        .map(|send| AudioSendChoice {
            id: send.id.clone(),
            label: send.label.clone(),
        })
        .collect()
}

fn audio_send_string_state(
    value: &str,
    setup: &manifold_core::audio_setup::AudioSetup,
) -> (Vec<ParamCardStringChoice>, String) {
    if setup.sends.is_empty() {
        return (
            vec![ParamCardStringChoice {
                label: "No audio sends".to_string(),
                value: String::new(),
                disabled: true,
            }],
            if value.is_empty() {
                "No audio sends".to_string()
            } else {
                "Missing send".to_string()
            },
        );
    }

    let choices = std::iter::once(ParamCardStringChoice {
        label: "First send".to_string(),
        value: String::new(),
        disabled: false,
    })
    .chain(setup.sends.iter().enumerate().map(|(index, send)| {
        ParamCardStringChoice {
            label: format!("{} · {}", index + 1, send.label),
            value: send.id.to_string(),
            disabled: false,
        }
    }))
    .collect();
    let display = if value.is_empty() {
        "First send".to_string()
    } else {
        setup
            .sends
            .iter()
            .enumerate()
            .find(|(_, send)| send.id.as_str() == value)
            .map(|(index, send)| format!("{} · {}", index + 1, send.label))
            .unwrap_or_else(|| "Missing send".to_string())
    };
    (choices, display)
}

fn find_string_node<'a>(
    nodes: &'a [manifold_core::effect_graph_def::EffectGraphNode],
    node_id: &manifold_core::NodeId,
) -> Option<&'a manifold_core::effect_graph_def::EffectGraphNode> {
    nodes.iter().find_map(|node| {
        (node.node_id == *node_id)
            .then_some(node)
            .or_else(|| node.group.as_ref().and_then(|group| find_string_node(&group.nodes, node_id)))
    })
}

fn graph_string_param_value(
    inst: &PresetInstance,
    sp_def: &manifold_core::preset_definition_registry::StringParamDef,
) -> (String, Option<String>) {
    let Some(catalog_def) = manifold_nodes::bundled_presets::bundled_preset_def(inst.effect_type()) else {
        return (sp_def.default_value.to_string(), None);
    };
    let graph = inst.graph.as_ref().unwrap_or(catalog_def.as_ref());
    let metadata = graph
        .preset_metadata
        .as_ref()
        .or(catalog_def.preset_metadata.as_ref());
    let binding = metadata.and_then(|m| {
        m.string_bindings
            .iter()
            .find(|b| b.id == sp_def.key)
    });
    let Some(binding) = binding else {
        return (sp_def.default_value.to_string(), None);
    };
    let value = match &binding.target {
        manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => find_string_node(&graph.nodes, node_id)
            .and_then(|node| node.params.get(param))
            .and_then(|value| match value {
                manifold_core::effect_graph_def::SerializedParamValue::String { value } => Some(value.clone()),
                _ => None,
            })
            .unwrap_or_else(|| binding.default_value.clone()),
        _ => binding.default_value.clone(),
    };
    (value, Some(binding.id.clone()))
}

/// Thin adapter: build a card config for each effect in `effects`, skipping
/// any whose preset def is missing. The real work is the unified
/// [`param_surface`].
pub(crate) fn effects_to_surfaces(
    effects: &[PresetInstance],
    osc_scope: OscScope<'_>,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    timing: (manifold_core::Bpm, f32),
) -> Vec<ParamSurface> {
    effects
        .iter()
        .enumerate()
        .filter_map(|(i, fx)| {
            param_surface(
                fx,
                manifold_core::preset_def::PresetKind::Effect,
                i,
                osc_scope,
                None,
                automation_latched,
                // Effects are always the curated outer card.
                SurfaceVisibility::CuratedCard,
                timing,
            )
        })
        .collect()
}

/// The empty generator card (no resolvable param source). Mirrors the old
/// `gen_params_to_surface` fallback exactly.
fn empty_generator_surface(inst: &PresetInstance) -> ParamSurface {
    ParamSurface {
        kind: ParamCardKind::Generator,
        title: inst.generator_type().to_string(),
        collapsed: false,
        effect_index: 0,
        // Stays blank (unlike the real-id arm in `param_surface` below):
        // zero rows means zero audio-mod rows, so nothing on this card ever
        // hosts a fire-meter lookup — there's no divergence risk to fix here.
        effect_id: manifold_core::EffectId::new(""),
        enabled: true,
        supports_envelopes: true,
        has_graph_mod: false,
        layer_id: None,
        modifier: None,
        rows: vec![],
        string_params: vec![],
        audio_sends: Vec::new(),
    }
}

/// BUG-080 D2: release-mode once-per-instance warn for a provisional
/// manifest reaching this seam. Shaped like the BUG-038 OSC-send throttle —
/// a plain "seen once" set is enough here, not a reconnect transition.
/// `debug_assert!` already screams in dev builds; this is the release-mode
/// signal that a load/ingest path skipped `reconcile_param_manifests()`.
fn warn_provisional_manifest_once(id: &manifold_core::EffectId) {
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<std::collections::HashSet<manifold_core::EffectId>>> =
        OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut warned = warned.lock().unwrap_or_else(|e| e.into_inner());
    if warned.insert(id.clone()) {
        log::warn!(
            "BUG-080: provisional manifest reached param_surface for effect_id={id:?} \
             — a load/ingest path skipped reconcile_param_manifests()"
        );
    }
}

/// THE projection (D1, `docs/WIDGET_TREE_DESIGN.md` — replaces the former
/// two-pass builder and its per-call id-to-index map). ONE manifest walk
/// builds [`ParamRow`]s directly — descriptor
/// (`spec`) verbatim from the manifest's `ParamSpecDef` fields, state
/// (`value`) alongside; display-value resolution (D7) happens here and
/// nowhere else. Returns `None` only for an effect whose preset def is
/// missing (skipped as a card); a generator with no source returns the empty
/// card.
fn param_surface(
    inst: &PresetInstance,
    kind: manifold_core::preset_def::PresetKind,
    effect_index: usize,
    osc_scope: OscScope<'_>,
    clip_string_params: Option<&std::collections::BTreeMap<String, String>>,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    visibility: SurfaceVisibility,
    timing: (manifold_core::Bpm, f32),
) -> Option<ParamSurface> {
    use manifold_core::preset_def::PresetKind;
    let preset_type = inst.effect_type();
    let reg_def = manifold_core::preset_definition_registry::try_get(preset_type);

    match kind {
        PresetKind::SceneModifier => {
            log::error!("scene modifier cards require an attached host and the scene modifier parameter resolver");
            return None;
        }
        PresetKind::Effect => {
            reg_def.as_deref()?; // skip cards for def-less effects
        }
        PresetKind::Generator => {
            if inst.params.is_empty() {
                // No resolvable param source (mirrors the old
                // graph-metadata-empty + registry-empty fallback chain,
                // now resolved once inside `build_param_manifest`).
                return Some(empty_generator_surface(inst));
            }
        }
    }

    // BUG-080 seam: a provisional manifest (built against an incomplete
    // registry, not yet reconciled) reaching UI row translation means a
    // load/ingest path skipped `reconcile_param_manifests()`. Loud in dev,
    // throttled-once in release. See docs/PARAM_MANIFEST_GATE_DESIGN.md D2.
    debug_assert!(
        !inst.manifest_provisional(),
        "BUG-080: provisional manifest reached param_surface — a load/ingest path \
         skipped reconcile_param_manifests() (effect_id={:?})",
        inst.id,
    );
    if inst.manifest_provisional() {
        warn_provisional_manifest_once(&inst.id);
    }

    // ── ONE walk over the manifest (PARAM_STORAGE_BOUNDARIES_DESIGN.md D4):
    // `inst.params` already carries every fact a row needs — descriptor
    // (spec) + state (exposed), id-keyed, insertion order IS card order —
    // because `build_param_manifest` resolved the registry-vs-graph-metadata
    // authority chain ONCE at instantiation/load. This walk reads that
    // result; it does not re-derive the authority chain or re-read a
    // per-instance graph override live (that override, `meta.params`, is a
    // save-time-derived shadow now — D12 — not a second live source).
    //
    // Which params become rows is the caller's explicit `visibility` choice
    // (scene-panel exposure convergence, card-visibility follow-up).
    // `CuratedCard` hides a scene-vocabulary auto-stamped param the curated
    // table marks `card_visible: false` (any role but performance) —
    // it stays a real, fully addressable manifest entry (OSC, Ableton, macros,
    // drivers all still resolve it by id), it just never becomes a CARD row.
    // `All` keeps every param (the Scene tab's full generator surface).
    // The per-frame value push no longer mirrors this filter: `sync_card_values`
    // hands the FULL manifest as an id-keyed channel and the card JOINS by id
    // (BUG-313), so a hidden param simply finds no row — there is no second
    // filter to drift out of alignment.
    let is_scene_card_candidate = visibility == SurfaceVisibility::CuratedCard && kind == PresetKind::Generator;
    let catalog_graph = (is_scene_card_candidate && inst.graph.is_none())
        .then(|| manifold_nodes::bundled_presets::bundled_preset_def(preset_type)).flatten();
    let scene_graph = is_scene_card_candidate
        .then(|| inst.graph.as_ref().or(catalog_graph.as_deref()))
        .flatten()
        .filter(|def| manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(def).is_some());
    let visible_params: Vec<&manifold_core::params::Param> = match visibility {
        SurfaceVisibility::CuratedCard if scene_graph.is_some() => inst.params.iter().collect(),
        SurfaceVisibility::CuratedCard => {
            let modifier_bindings = inst.graph.as_ref().and_then(|graph| graph.preset_metadata.as_ref());
            inst.params.iter().filter(|p| p.spec.card_visible && !modifier_bindings.is_some_and(|metadata|
                metadata.bindings.iter().any(|binding| binding.id == p.id()
                    && matches!(binding.target, manifold_core::effect_graph_def::BindingTarget::SceneModifier { .. })))).collect()
        }
        SurfaceVisibility::All => inst.params.iter().collect(),
    };

    let row_index_of: ahash::AHashMap<String, usize> =
        visible_params.iter().enumerate().map(|(i, p)| (p.id().to_string(), i)).collect();

    let mut rows: Vec<ParamRow> = visible_params
        .iter()
        .map(|p| {
            let id = p.id().to_string();
            let osc_address = match osc_scope {
                OscScope::Master => {
                    manifold_core::preset_definition_registry::get_osc_address_by_id(
                        preset_type,
                        &id,
                    )
                }
                OscScope::Layer(lid) => {
                    manifold_core::preset_definition_registry::get_osc_address_for_layer_by_id(
                        preset_type,
                        lid,
                        &id,
                    )
                }
            };
            let abl_mapping = inst.ableton_mappings.as_ref().and_then(|mappings| {
                if id.is_empty() {
                    return None;
                }
                mappings.iter().find(|m| m.param_id == id)
            });
            let ableton_display = abl_mapping.map(|mapping| AbletonMappingDisplay {
                macro_name: mapping.address.macro_name.clone(),
                track_name: mapping.address.track_name.clone(),
                device_name: mapping.address.device_name.clone(),
                status: crate::ui_translate::ableton_mapping_status_to_ui(mapping.status),
                inverted: mapping.inverted,
            });
            let ableton_range = abl_mapping.map(|m| (m.range_min, m.range_max));
            let value_labels = if p.spec.value_labels.is_empty() {
                None
            } else {
                Some(p.spec.value_labels.clone())
            };
            let stored = inst.modulator_stored(&id);
            ParamRow {
                id: std::borrow::Cow::Owned(id),
                spec: RowSpec {
                    tooltip: p.spec.tooltip.clone(),
                    name: p.spec.name.clone(),
                    min: p.slider_span().0,
                    max: p.slider_span().1,
                    default: p.spec.default_value,
                    whole_numbers: p.spec.whole_numbers,
                    is_angle: p.spec.is_angle,
                    is_toggle: p.spec.is_toggle,
                    is_trigger: p.spec.is_trigger,
                    is_trigger_gate: p.spec.is_trigger_gate,
                    value_labels,
                    unit: p.spec.unit,
                    section: p.spec.section.clone(),
                    material_role: p.spec.material_role.map(super::material::role),
                    inactive_reason: None,
                    scene: p.spec.scene,
                    limit: p.drag_limit(),
                    disabled: None,
                },
                // D7: display-value resolution decided here — base/effective
                // straight off the manifest slot, `driven` false (state_sync
                // has no wire-fed presentation case; only the editor snapshot
                // path sets it).
                value: RowValue { base: p.base, effective: p.value, exposed: p.exposed, driven: false },
                modulation: RowMod::default(),
                mapping: RowMapping { osc_address, ableton_display, ableton_range, mappable: true },
                scene_addr: None,
                rgb_members: None,
                modulator_stored: stored,
                audio: Default::default(),
                clip_trigger: None,
            }
        })
        .collect();
    let n = rows.len();

    let mod_rows = build_card_modulation(
        inst,
        n,
        |id| row_index_of.get(id).copied(),
        automation_latched,
        timing,
    );
    for (row, rm) in rows.iter_mut().zip(mod_rows) {
        row.modulation = rm;
    }
    for am in inst.audio_mods.iter().flatten() {
        if !am.enabled {
            continue;
        }
        let Some(pi) = row_index_of.get(am.param_id.as_ref()).copied() else {
            continue;
        };
        let is_fire = rows[pi].spec.is_trigger && !rows[pi].spec.is_trigger_gate;
        rows[pi].audio = audio_row_state(am, is_fire);
    }

    if let Some(def) = scene_graph {
        super::scene_card::scene_card_rows(&mut rows, inst, def);
    }

    // String params are sourced from the registry def. Graph-backed audio-send
    // selectors use the instance graph for both effects and generators; other
    // generator strings retain their clip-owned value path.
    let string_params: Vec<ParamCardStringInfo> = match kind {
        PresetKind::Generator => reg_def
            .as_deref()
            .map(|def| {
                def.string_param_defs
                    .iter()
                    .map(|sp_def| {
                        let value = if sp_def.key == "audioSend" {
                            graph_string_param_value(inst, sp_def).0
                        } else {
                            clip_string_params
                                .and_then(|m| m.get(sp_def.key))
                                .cloned()
                                .unwrap_or_else(|| sp_def.default_value.to_string())
                        };
                        let display_value = (scene_graph.is_some() && sp_def.is_file_path)
                            .then(|| std::path::Path::new(&value).file_name()
                                .map(|name| name.to_string_lossy().into_owned()))
                            .flatten();
                        ParamCardStringInfo {
                            name: sp_def.name.to_string(),
                            key: sp_def.key.to_string(),
                            value,
                            use_dropdown: sp_def.use_dropdown,
                            effect_id: None,
                            binding_id: None,
                            dropdown_choices: Vec::new(),
                            display_value,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        PresetKind::Effect => reg_def
            .as_deref()
            .map(|def| {
                def.string_param_defs
                    .iter()
                    .map(|sp_def| {
                        let (value, binding_id) = graph_string_param_value(inst, sp_def);
                        ParamCardStringInfo {
                            name: sp_def.name.to_string(),
                            key: sp_def.key.to_string(),
                            value,
                            use_dropdown: sp_def.use_dropdown,
                            effect_id: Some(inst.id.clone()),
                            binding_id,
                            dropdown_choices: Vec::new(),
                            display_value: None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        PresetKind::SceneModifier => return None,
    };

    let (card_kind, effect_id, enabled, collapsed, has_graph_mod) = match kind {
        PresetKind::SceneModifier => return None,
        PresetKind::Effect => (
            ParamCardKind::Effect,
            inst.id.clone(),
            inst.enabled,
            inst.collapsed,
            inst.graph.is_some(),
        ),
        PresetKind::Generator => (
            ParamCardKind::Generator,
            // Real `inst.id`, not a blanked placeholder (fixed 2026-07-11):
            // this is the SAME id `build_card_modulation` already used for
            // its own lookups (see that fn's doc comment) and the SAME id
            // the content thread hashes into a fire-meter key
            // (`fire_meter_key_for_param`) — a blanked display id here meant
            // the UI's lookup key could never match the content thread's,
            // so a generator card's audio-mod meters never resolved.
            inst.id.clone(),
            true,
            false,
            // PRESET_LIBRARY_DESIGN D3/P4: a generator's per-card divergence
            // is the SAME `graph.is_some()` bit as an effect's (graph-home
            // unification put both on `PresetInstance`) — this was
            // hardcoded `false` (a pre-P4 gap that permanently suppressed
            // the MOD badge on generator cards regardless of actual
            // divergence), fixed to read the real state like the Effect arm
            // above.
            inst.graph.is_some(),
        ),
    };

    Some(ParamSurface {
        kind: card_kind,
        effect_index,
        effect_id,
        // A project-embedded (forked) preset's `display_name` — sourced from
        // `reg_def`, the same catalog-overlay-aware lookup the rows above
        // used — carries its own human name directly (D2: ids are now
        // display-based, so no id-format parsing is needed to render one).
        // Falls back to the static registry name for stock/user presets not
        // (yet) reflected in the overlay snapshot.
        title: reg_def.as_deref().map(|d| d.display_name.clone()).unwrap_or_else(|| {
            manifold_core::preset_type_registry::display_name(preset_type).to_string()
        }),
        enabled,
        collapsed,
        supports_envelopes: true,
        string_params,
        layer_id: None,
        modifier: None,
        rows,
        has_graph_mod,
        audio_sends: Vec::new(),
    })
}

/// Thin adapter: build the generator card config via the unified
/// [`preset_to_config`]. The generator branch always yields a config (a real
/// one, or the empty fallback when no param source resolves), so the `expect`
/// never fires.
pub(crate) fn gen_params_to_surface(
    gp: &manifold_core::effects::PresetInstance,
    layer_id: &str,
    clip_string_params: Option<&std::collections::BTreeMap<String, String>>,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    visibility: SurfaceVisibility,
    timing: (manifold_core::Bpm, f32),
) -> ParamSurface {
    let mut surface = param_surface(
        gp,
        manifold_core::preset_def::PresetKind::Generator,
        0,
        OscScope::Layer(layer_id),
        clip_string_params,
        automation_latched,
        visibility,
        timing,
    )
    .expect("generator param_surface always yields a config");
    surface.layer_id = Some(manifold_core::LayerId::new(layer_id));
    surface
}

/// The folded section titles of a curated generator card when it is a scene
/// card, read from the same graph its curation read; `None` otherwise.
pub(crate) fn scene_card_folds(gp: &manifold_core::effects::PresetInstance, card: &ParamSurface) -> Option<Vec<String>> {
    let catalog = gp
        .graph
        .is_none()
        .then(|| manifold_nodes::bundled_presets::bundled_preset_def(gp.effect_type()))
        .flatten();
    let def = gp.graph.as_ref().or(catalog.as_deref())?;
    super::scene_card::folded_sections(gp, def, &card.rows)
}

fn scene_ref_for_vm(
    nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
    scene_doc_id: u32,
    scope: &[manifold_core::NodeId],
) -> Option<manifold_core::scene_modifier_preset::SceneNodeRef> {
    for node in nodes {
        if node.id == scene_doc_id
            && node.type_id == manifold_nodes_scene::node_graph::scene_vm::RENDER_SCENE_TYPE_ID
        {
            return Some(manifold_core::scene_modifier_preset::SceneNodeRef {
                scope: scope.to_vec(),
                node: node.node_id.clone(),
            });
        }
        if let Some(group) = &node.group {
            let mut child_scope = scope.to_vec();
            child_scope.push(node.node_id.clone());
            if let Some(scene) = scene_ref_for_vm(&group.nodes, scene_doc_id, &child_scope) {
                return Some(scene);
            }
        }
    }
    None
}

fn modifier_picker_reason(
    error: &manifold_node_engine::load::expand::SceneModifierExpandError,
) -> String {
    log::debug!("scene modifier picker admission rejected recipe: {error}");
    use manifold_node_engine::load::expand::SceneModifierExpandError;
    match error {
        SceneModifierExpandError::UnsupportedCoordinateFrame { detail, .. }
            if detail.contains("imported scene bounds") =>
        {
            "Requires imported scene bounds".into()
        }
        SceneModifierExpandError::UnsupportedCoordinateFrame { detail, .. }
            if detail.contains("direct static glTF") =>
        {
            "Requires an imported static mesh".into()
        }
        SceneModifierExpandError::ConflictingSource { .. } => "Conflicting scene source".into(),
        SceneModifierExpandError::MissingInput { .. } => "Missing scene input".into(),
        SceneModifierExpandError::UnsupportedEndpoint { .. } => "Unsupported scene output".into(),
        SceneModifierExpandError::CapacityExceeded { .. } => "Scene exceeds modifier capacity".into(),
        SceneModifierExpandError::MissingScene { .. } => "Scene target unavailable".into(),
        _ => "Not applicable to this scene".into(),
    }
}

/// The same disk/project catalog supplies picker entries and attached snapshots.
/// Applicability is checked here with the same preparation and expansion
/// contract used by the structural add command, so the picker does not promise
/// recipes that attachment will reject.
pub(crate) fn modifier_picker_entries(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    vm: &manifold_nodes_scene::node_graph::scene_vm::SceneVm,
) -> Vec<manifold_ui::param_surface::ModifierPickerEntry> {
    scene_recipe_picker_entries(def, vm, false)
}

pub(crate) fn force_picker_entries(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    vm: &manifold_nodes_scene::node_graph::scene_vm::SceneVm,
) -> Vec<manifold_ui::param_surface::ModifierPickerEntry> {
    scene_recipe_picker_entries(def, vm, true)
}

fn scene_recipe_picker_entries(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    vm: &manifold_nodes_scene::node_graph::scene_vm::SceneVm,
    forces: bool,
) -> Vec<manifold_ui::param_surface::ModifierPickerEntry> {
    use manifold_node_engine::load::preset_loader::SCENE_MODIFIER_CATALOG;
    use manifold_ui::param_surface::ModifierPickerEntry;
    let catalog = SCENE_MODIFIER_CATALOG.load();
    let mut entries: Vec<_> = catalog.entries().filter_map(|(id, json)| {
        if !catalog.is_browser_visible(&id) { return None; }
        let recipe: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(&json).ok()?;
        if manifold_core::scene_modifier_preset::is_force_recipe(&recipe) != forces {
            return None;
        }
        let metadata = recipe.preset_metadata.as_ref()?;
        if !metadata.available { return None; }
        let attachment = metadata.scene_modifier.as_ref()?;
        let disabled = if vm.multiple_scenes {
            Some("Select a graph with one scene".to_string())
        } else if attachment.singleton && def.scene_modifiers.iter().any(|instance|
            instance.graph.preset_metadata.as_ref().is_some_and(|m| m.id == metadata.id)) {
            Some("Already applied".to_string())
        } else {
            let Some(scene) = scene_ref_for_vm(&def.nodes, vm.scene_root_node_id, &[]) else {
                return Some(ModifierPickerEntry {
                    preset_id: id.to_string(),
                    label: metadata.display_name.clone(),
                    disabled: Some("Scene root is unavailable".to_string()),
                });
            };
            let instance = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
                def,
                &recipe,
                manifold_core::NodeId::new(format!("picker:{id}")),
                scene,
                manifold_core::scene_modifier_preset::SceneTargetSelection::AllObjects,
            ).and_then(|instance|
                manifold_nodes_scene::node_graph::scene_modifier_authoring::validate_new_scene_modifier(def, &instance)
                    .map(|()| instance)
            );
            instance.err().map(|error| modifier_picker_reason(&error))
        };
        Some(ModifierPickerEntry { preset_id: id.to_string(), label: metadata.display_name.clone(), disabled })
    }).collect();
    entries.sort_by(|a, b| a.label.cmp(&b.label).then(a.preset_id.cmp(&b.preset_id)));
    entries
}

/// A card owns rows through stable host-to-instance bindings. Titles and
/// sections are presentation only; modulation still addresses the generator.
pub(crate) fn is_force_surface(
    surface: &ParamSurface,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
) -> bool {
    surface.modifier.as_ref().is_some_and(|info| {
        def.scene_modifiers.iter().any(|instance| instance.id == info.instance_id
            && manifold_core::scene_modifier_preset::is_force_recipe(&instance.graph))
    })
}

pub(crate) fn modifier_surfaces(
    gp: &manifold_core::effects::PresetInstance,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    _vm: &manifold_nodes_scene::node_graph::scene_vm::SceneVm,
    layer_id: &str,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    timing: (manifold_core::Bpm, f32),
) -> Vec<ParamSurface> {
    use manifold_core::effect_graph_def::BindingTarget;
    use manifold_ui::param_surface::{ModifierCardInfo, ModifierObjectOption, ModifierObjectRef};
    use manifold_core::scene_modifier_preset::SceneTargetSelection;
    let full = gen_params_to_surface(gp, layer_id, None, automation_latched,
        SurfaceVisibility::All, timing);
    let bindings = def.preset_metadata.as_ref().map(|m| m.bindings.as_slice()).unwrap_or(&[]);
    def.scene_modifiers.iter().enumerate().filter_map(|(index, instance)| {
        let metadata = instance.graph.preset_metadata.as_ref()?;
        let recipe = metadata.scene_modifier.as_ref()?;
        let local_id = |host_id: &str| bindings.iter().find_map(|binding| {
            if binding.id != host_id { return None; }
            match &binding.target {
                BindingTarget::SceneModifier { modifier_id, param_id } if modifier_id == &instance.id => Some(param_id.as_str()),
                _ => None,
            }
        });
        let enabled_row = full.rows.iter().find(|row| local_id(row.id.as_ref()) == Some(recipe.enabled_param.as_str()));
        // Bypass-style controls keep the compact card-header toggle. A
        // recipe-specific semantic control (SceneLoop's Camera Travel) stays
        // as an ordinary labeled toggle row so its meaning remains visible.
        let enabled_row_in_body = enabled_row.is_some_and(|row| row.spec.name != "Enabled");
        let enabled = modifier_enabled_value_for_binding(gp, bindings, instance, &recipe.enabled_param);
        let force = manifold_core::scene_modifier_preset::is_force_recipe(&instance.graph);
        let force_objects = if force {
            Some(manifold_node_engine::load::expand::force_objects_for_authoring(def, &instance.scene)
                .unwrap_or_else(|error| {
                    log::error!("force {} targets unavailable: {error}", instance.id);
                    Vec::new()
                }))
        } else { None };
        let object_targeting = recipe.stages.iter().any(|stage| {
            stage.outputs.iter().any(|output| matches!(
                output.endpoint,
                manifold_core::scene_modifier_preset::SceneEndpoint::Transform
                    | manifold_core::scene_modifier_preset::SceneEndpoint::Instances
                    | manifold_core::scene_modifier_preset::SceneEndpoint::Vertices
                    | manifold_core::scene_modifier_preset::SceneEndpoint::Acceleration
            ))
        });
        let mut rows: Vec<_> = full.rows.iter().filter(|row| local_id(row.id.as_ref()).is_some_and(|id|
            (enabled_row_in_body || id != recipe.enabled_param)
                && !recipe.preparation_params.iter().any(|p| p == id))).cloned().collect();
        for row in &mut rows { row.scene_addr = None; }
        Some(ParamSurface {
            kind: ParamCardKind::Effect,
            title: manifold_nodes_scene::node_graph::scene_items::scene_modifier_display_name(def, instance)
                .unwrap_or_else(|| metadata.display_name.clone()),
            rows,
            string_params: vec![],
            audio_sends: Vec::new(),
            modifier: Some(ModifierCardInfo {
                instance_id: instance.id.clone(),
                layer_id: manifold_core::LayerId::new(layer_id),
                params_owner: gp.id.clone(),
                enabled_label:enabled_row.map(|row| row.spec.name.clone()).unwrap_or_else(|| "Enabled".into()),
                stack_index: index,
                stack_len: def.scene_modifiers.len(),
                targets_all: object_targeting && matches!(instance.targets, SceneTargetSelection::AllObjects),
                objects: if object_targeting { manifold_nodes_scene::node_graph::scene_modifier_authoring::scene_modifier_objects(def, &instance.scene)
                    .unwrap_or_else(|error| {
                        log::error!("scene modifier {} object selection unavailable: {error}", instance.id);
                        Vec::new()
                    }).into_iter().map(|object| {
                        let selected = match &instance.targets {
                            SceneTargetSelection::AllObjects => true,
                            SceneTargetSelection::Explicit { objects } => objects.contains(&object),
                        };
                        ModifierObjectOption {
                            label: if force_objects.as_ref().is_some_and(|eligible| !eligible.contains(&object)) {
                                format!("{} (no physics)", modifier_object_label(def, &object))
                            } else { modifier_object_label(def, &object) },
                            object: ModifierObjectRef { scope: object.scope, node: object.node },
                            selected,
                        }
                    }).collect() } else { Vec::new() },
            }),
            effect_index: 0,
            effect_id: manifold_core::EffectId::new(format!("scene_modifier:{}", instance.id)),
            enabled,
            collapsed: false,
            supports_envelopes: true,
            has_graph_mod: crate::modifier_preset::has_graph_mod(def, &instance.graph),
            layer_id: None,
        })
    }).collect()
}


fn modifier_enabled_value(
    gp: &manifold_core::effects::PresetInstance,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    instance: &manifold_core::scene_modifier_preset::SceneModifierInstanceDef,
) -> bool {
    let Some(recipe) = instance
        .graph
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
    else {
        return false;
    };
    let bindings = def
        .preset_metadata
        .as_ref()
        .map(|metadata| metadata.bindings.as_slice())
        .unwrap_or(&[]);
    modifier_enabled_value_for_binding(gp, bindings, instance, &recipe.enabled_param)
}

fn modifier_enabled_value_for_binding(
    gp: &manifold_core::effects::PresetInstance,
    bindings: &[manifold_core::effect_graph_def::BindingDef],
    instance: &manifold_core::scene_modifier_preset::SceneModifierInstanceDef,
    enabled_param: &str,
) -> bool {
    let Some(binding) = bindings.iter().find(|binding| {
        matches!(
            &binding.target,
            manifold_core::effect_graph_def::BindingTarget::SceneModifier {
                modifier_id,
                param_id,
            } if modifier_id == &instance.id && param_id == enabled_param
        )
    }) else {
        let prepared = instance
            .graph
            .preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_modifier.as_ref())
            .is_some_and(|recipe| recipe.preparation_params.iter().any(|param| param == enabled_param));
        if prepared {
            return instance
                .graph
                .preset_metadata
                .as_ref()
                .and_then(|metadata| metadata.params.iter().find(|param| param.id == enabled_param))
                .is_some_and(|param| param.default_value > 0.5);
        }
        return false;
    };
    let Some(param) = gp.params.get(binding.id.as_str()) else {
        return false;
    };
    manifold_core::effects::apply_card_reshape(
        param.base,
        param.spec.min,
        param.spec.max,
        param.spec.invert,
        param.spec.curve,
        binding.scale,
        binding.offset,
    ) > 0.5
}

fn modifier_object_label(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    object: &manifold_core::scene_modifier_preset::SceneNodeRef,
) -> String {
    let mut nodes = def.nodes.as_slice();
    let mut labels = Vec::new();
    for id in &object.scope {
        let Some(node) = nodes.iter().find(|node| &node.node_id == id) else { return object.node.to_string(); };
        labels.push(node.handle.clone().unwrap_or_else(|| id.to_string()));
        let Some(group) = node.group.as_ref() else { return object.node.to_string(); };
        nodes = &group.nodes;
    }
    let label = nodes.iter().find(|node| node.node_id == object.node)
        .and_then(|node| node.handle.clone()).unwrap_or_else(|| object.node.to_string());
    labels.push(label);
    labels.join(" / ")
}

/// Write a human-readable description of a macro mapping target over `out`,
/// reusing its allocation.
pub(crate) fn write_macro_mapping(
    out: &mut String,
    target: &manifold_core::MacroMappingTarget,
    project: &manifold_core::project::Project,
) {
    use manifold_core::MacroMappingTarget;
    use std::fmt::Write;
    out.clear();
    let _ = match target {
        MacroMappingTarget::MasterOpacity => out.write_str("Master Opacity"),
        MacroMappingTarget::Effect {
            effect_id,
            param_id,
        } => {
            let Some(fx) = project.find_effect_by_id(effect_id) else {
                out.push_str("Effect → ?");
                return;
            };
            let effect_type = fx.effect_type();
            // Effect display name is type-level template metadata (a boundary
            // read); the param name comes off the LIVE manifest so user-added /
            // glb params resolve instead of rendering "?" (was a registry
            // id-lookup miss, the UI twin of the P4 blind spot).
            let def = manifold_core::preset_definition_registry::try_get(effect_type);
            let effect_name = def.as_deref().map_or(effect_type.as_str(), |d| d.display_name.as_str());
            let param_name = fx
                .params
                .get(param_id.as_ref())
                .map_or("?", |p| p.spec.name.as_str());
            // Prefix with the owning layer's name; master effects have none.
            match project.layer_id_for_effect(effect_id) {
                Some(layer_id) => {
                    let layer_name = project
                        .timeline
                        .layers
                        .iter()
                        .find(|l| l.layer_id == layer_id)
                        .map(|l| l.name.as_str())
                        .unwrap_or("?");
                    write!(out, "{} {} → {}", layer_name, effect_name, param_name)
                }
                None => write!(out, "{} → {}", effect_name, param_name),
            }
        }
        MacroMappingTarget::LayerOpacity { layer_id } => {
            let layer_name = project
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id == *layer_id)
                .map(|l| l.name.as_str())
                .unwrap_or(layer_id.as_str());
            write!(out, "{} Opacity", layer_name)
        }
        MacroMappingTarget::GenParam { layer_id, param_id } => {
            let layer = project
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id == *layer_id);
            let layer_name = layer.map(|l| l.name.as_str()).unwrap_or("?");
            // Param name off the LIVE manifest (user-added / glb params resolve).
            let param_name = layer
                .and_then(|l| l.gen_params())
                .and_then(|gp| gp.params.get(param_id.as_ref()))
                .map_or("?", |p| p.spec.name.as_str());
            write!(out, "{} Gen → {}", layer_name, param_name)
        }
    };
}

#[cfg(test)]
mod param_label_tests {
    use super::*;
    use manifold_core::MacroMappingTarget;
    use manifold_core::effects::PresetInstance;
    use manifold_core::params::{Param, ParamManifest};

    fn user_spec(id: &str, name: &str) -> manifold_core::effect_graph_def::ParamSpecDef {
        manifold_core::effect_graph_def::ParamSpecDef {
            tooltip: None,
            id: id.to_string(),
            name: name.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: manifold_core::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
            material_role: None,
            limit: Default::default(),
            unit: None,
            scene: None,
        }
    }

    /// P5: a macro-mapping label resolves a param's display name from the LIVE
    /// manifest, so a user-added param shows its name instead of "?" (before,
    /// the registry id-lookup missed it — the UI twin of the P4
    /// blind spot).
    #[test]
    fn describe_macro_mapping_uses_live_manifest_param_name() {
        let mut project = manifold_core::project::Project::default();
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::BLOOM);
        fx.params =
            ParamManifest::from_params(vec![Param::user_added(user_spec("user_glow", "Glow Amount"))]);
        let effect_id = fx.id.clone();
        project.settings.master_effects.push(fx);

        let target = MacroMappingTarget::Effect {
            effect_id,
            param_id: std::borrow::Cow::Owned("user_glow".to_string()),
        };
        let mut label = String::new();
        write_macro_mapping(&mut label, &target, &project);
        assert!(
            label.contains("Glow Amount"),
            "label must show the live param name, got {label:?}"
        );
        assert!(!label.contains('?'), "label must not fall back to ?, got {label:?}");
    }
}

#[cfg(test)]
mod audio_send_projection_tests {
    use super::*;
    use manifold_core::audio_setup::{AudioSend, AudioSetup};

    #[test]
    fn audio_send_choices_keep_empty_first_send_and_follow_stable_ids() {
        let first = AudioSend::new("Music");
        let second = AudioSend::new("Music");
        let selected = second.id.to_string();
        let mut setup = AudioSetup::default();
        setup.sends = vec![first.clone(), second.clone()];

        let (choices, display) = audio_send_string_state("", &setup);
        assert_eq!(choices[0].value, "", "First send uses the automatic empty payload");
        assert_eq!(display, "First send");
        assert_eq!(choices[1].label, "1 · Music");
        assert_eq!(choices[2].label, "2 · Music");

        let (_, selected_display) = audio_send_string_state(&selected, &setup);
        assert_eq!(selected_display, "2 · Music");

        setup.sends = vec![second];
        let (_, reordered_display) = audio_send_string_state(&selected, &setup);
        assert_eq!(reordered_display, "1 · Music");

        setup.sends.clear();
        let (_, missing_display) = audio_send_string_state(&selected, &setup);
        assert_eq!(missing_display, "Missing send");
    }

    #[test]
    fn generator_audio_send_surface_reads_graph_value_and_display() {
        let mut generator = PresetInstance::new_generator(
            manifold_core::PresetTypeId::from_string("Oscilloscope".to_string()),
        );
        generator.init_defaults();
        let mut graph = manifold_nodes::bundled_presets::bundled_preset_def(generator.effect_type())
            .expect("Oscilloscope generator preset is bundled").as_ref().clone();
        let first = AudioSend::new("Music");
        let second = AudioSend::new("Music");
        let selected_id = second.id.to_string();
        graph
            .nodes
            .iter_mut()
            .find(|node| node.node_id.as_str() == "waveform")
            .expect("Oscilloscope waveform node")
            .params
            .insert(
                "send".to_string(),
                manifold_core::effect_graph_def::SerializedParamValue::String {
                    value: selected_id.clone(),
                },
            );
        generator.graph = Some(graph);

        let mut setup = AudioSetup::default();
        setup.sends = vec![first, second];
        let mut config = param_surface(
            &generator,
            manifold_core::preset_def::PresetKind::Generator,
            0,
            OscScope::Layer("oscilloscope-layer"),
            None,
            &[],
            SurfaceVisibility::CuratedCard,
            (manifold_core::Bpm(120.0), 0.0),
        )
        .expect("generator surface");
        attach_audio_sends(std::slice::from_mut(&mut config), &setup);

        let audio_send = config
            .string_params
            .iter()
            .find(|param| param.key == "audioSend")
            .expect("Audio Send string row");
        assert_eq!(audio_send.value, selected_id);
        assert_eq!(audio_send.display_value.as_deref(), Some("2 · Music"));
        assert_eq!(audio_send.dropdown_choices[2].value, audio_send.value);
    }
}

#[cfg(test)]
mod modifier_audio_projection_tests;

#[cfg(test)]
mod sync_card_values_tests {
    //! The extraction proof for `sync_card_values`: a param value changed in
    //! the (UI-local) project after the inspector was configured must reach
    //! the card's on-tree value text through `sync_card_values` alone — no
    //! structural re-sync, no rebuild. This is the exact call the
    //! graph-editor window's present path now makes every frame
    //! (`app_render.rs::present_graph_editor_window`), so the test guards the
    //! editor-window slider-freeze fix, not just the helper.
    use super::*;
    use super::super::inspector::sync_inspector_data;
    use manifold_core::PresetTypeId;
    use crate::app::SelectionState;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::effect_graph_def::ParamSpecDef;

    fn user_spec(id: &str, name: &str) -> manifold_core::effect_graph_def::ParamSpecDef {
        manifold_core::effect_graph_def::ParamSpecDef {
            tooltip: None,
            id: id.to_string(),
            name: name.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: manifold_core::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
            material_role: None,
            limit: Default::default(),
            unit: None,
            scene: None,
        }
    }

    fn tree_has_text(ui: &UIRoot, needle: &str) -> bool {
        ui.tree
            .all_nodes()
            .any(|n| n.text.as_deref() == Some(needle))
    }

    #[test]
    fn project_param_change_reaches_card_value_text_via_sync_card_values() {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params =
            ParamManifest::from_params(vec![Param::user_added(user_spec("user_glow", "Glow Amount"))]);
        project.settings.master_effects.push(fx);

        // Configure + build exactly as the structural sync does, at the
        // pre-change value (0.5 → "0.50" via `format_param_value`'s `{:.2}`).
        let mut ui = UIRoot::new();
        let selection = SelectionState::default();
        sync_inspector_data(&mut ui, &project, None, &selection, &[], None);
        ui.build_inspector_in_rect(manifold_ui::Rect::new(0.0, 0.0, 640.0, 2000.0));
        assert!(
            tree_has_text(&ui, "0.50"),
            "baseline: the configured card must show the pre-change value"
        );

        // A modulation-style write to the local project, then ONLY the
        // value-sync call — no configure, no rebuild.
        project.settings.master_effects[0]
            .params
            .get_mut("user_glow")
            .expect("user_glow param")
            .value = 0.75;
        sync_card_values(&mut ui, &project, None);

        assert!(
            tree_has_text(&ui, "0.75"),
            "sync_card_values must push the new value onto the already-built card"
        );
        // No "stale text is gone" assertion: "0.50" legitimately appears on
        // other widgets (e.g. mapping trim fields seeded from the same
        // default), so disappearance is not a sound oracle here.
    }

    #[test]
    fn project_param_range_edit_reaches_built_card_slider_via_sync_card_values() {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = ParamManifest::from_params(vec![Param::user_added(user_spec(
            "user_glow",
            "Glow Amount",
        ))]);
        project.settings.master_effects.push(fx);

        // Configure + build at the initial range (0.0–1.0).
        let mut ui = UIRoot::new();
        let selection = SelectionState::default();
        sync_inspector_data(&mut ui, &project, None, &selection, &[], None);
        ui.build_inspector_in_rect(manifold_ui::Rect::new(0.0, 0.0, 640.0, 2000.0));

        // A calibration edit: change the param's min/max in the manifest.
        project.settings.master_effects[0]
            .params
            .get_mut("user_glow")
            .expect("user_glow param")
            .spec = ParamSpecDef {
                id: "user_glow".to_string(),
                name: "Glow Amount".to_string(),
                min: 10.0,  // Changed from 0.0
                max: 100.0, // Changed from 1.0
                default_value: 50.0,
                ..ParamSpecDef::default()
            };

        // Only the value-sync call — no configure, no rebuild.
        sync_card_values(&mut ui, &project, None);

        // Verify the built card's row spec reflects the new range.
        // This is the real oracle: the stored spec.min/max drive the slider's
        // normalization math, so a stale spec would keep using 0..1 despite
        // the manifest now saying 10..100.
        let effect_card = ui.inspector.master_effect_mut(0).expect("effect card exists");
        assert_eq!(
            effect_card.rows[0].spec.min, 10.0,
            "sync_card_values must update the built row's spec.min to match the manifest edit"
        );
        assert_eq!(
            effect_card.rows[0].spec.max, 100.0,
            "sync_card_values must update the built row's spec.max to match the manifest edit"
        );
    }
}

#[cfg(test)]
mod consolidation_tests {
    fn fixture() -> manifold_core::effect_graph_def::EffectGraphDef {
        serde_json::from_str(manifold_nodes::testkit::assets::TESTS_FIXTURES_SCENE_MODIFIERS_NESTED_MULTIMATERIAL_V2_JSON)
        .unwrap()
    }

    fn picker_entry(
        def: &manifold_core::effect_graph_def::EffectGraphDef,
        preset_id: &str,
    ) -> manifold_ui::param_surface::ModifierPickerEntry {
        let vm = manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(def).unwrap();
        super::modifier_picker_entries(def, &vm)
            .into_iter()
            .find(|entry| entry.preset_id == preset_id)
            .unwrap_or_else(|| panic!("picker entry {preset_id} missing"))
    }

    #[test]
    fn modifier_picker_omits_retired_factory_combinations() {
        let def = fixture();
        let vm = manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(&def).unwrap();
        let entries = super::modifier_picker_entries(&def, &vm);
        for id in ["SurfacePeel", "OrderedRecon", "SurfaceWaves", "SpatialEchoes"] {
            assert!(entries.iter().any(|entry| entry.preset_id == id), "missing {id}");
        }
        for id in ["SurfacePeelHit", "OrderedReconHit", "MaskedPeel", "WavesEchoes"] {
            assert!(!entries.iter().any(|entry| entry.preset_id == id), "retired {id} still in picker");
        }
    }

    #[test]
    fn picker_distinguishes_procedural_and_imported_calibration_support() {
        let mut procedural = fixture();
        procedural
            .preset_metadata
            .as_mut()
            .unwrap()
            .scene_bounds = None;
        let procedural_loop = picker_entry(&procedural, "SceneLoop");
        assert!(procedural_loop
            .disabled
            .as_deref()
            .is_some_and(|reason| reason.contains("imported scene bounds")));

        let imported_loop = picker_entry(&fixture(), "SceneLoop");
        assert!(imported_loop.disabled.is_none(), "imported bounds admit SceneLoop: {imported_loop:?}");
    }

    #[test]
    fn picker_rejects_pre_modified_scene_and_conflicting_source_graph() {
        let mut modified = fixture();
        let recipe = manifold_nodes::bundled_presets::bundled_preset_def(
            &manifold_core::PresetTypeId::new("SceneFog"),
        )
        .unwrap();
        let instance = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
            &modified,
            recipe.as_ref(),
            "existing-fog".into(),
            manifold_core::scene_modifier_preset::SceneNodeRef {
                scope: vec![],
                node: "scan_render".into(),
            },
            manifold_core::scene_modifier_preset::SceneTargetSelection::AllObjects,
        )
        .unwrap();
        modified = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &modified,
            0,
            instance,
        )
        .unwrap()
        .graph;
        let fog = picker_entry(&modified, "SceneFog");
        assert_eq!(fog.disabled.as_deref(), Some("Already applied"));

        let mut conflicting = fixture();
        let scene_id = conflicting
            .nodes
            .iter()
            .find(|node| node.type_id == manifold_nodes_scene::node_graph::scene_vm::RENDER_SCENE_TYPE_ID)
            .expect("fixture render scene")
            .id;
        let next_id = conflicting.nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
        let mut existing_render_mode = conflicting
            .nodes
            .first()
            .cloned()
            .expect("fixture node");
        existing_render_mode.id = next_id;
        existing_render_mode.node_id = "existing_render_mode".into();
        existing_render_mode.type_id = "node.render_mode".into();
        existing_render_mode.handle = Some("existing_render_mode".into());
        existing_render_mode.params.clear();
        existing_render_mode.group = None;
        conflicting.nodes.push(existing_render_mode);
        conflicting.wires.push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: next_id,
            from_port: "render_mode".into(),
            to_node: scene_id,
            to_port: "render_mode".into(),
        });
        let render_mode = picker_entry(&conflicting, "RenderMode");
        assert!(render_mode
            .disabled
            .as_deref()
            .is_some_and(|reason| reason == "Conflicting scene source"));
    }

    #[test]
    fn modifier_target_chrome_follows_recipe_output_scope() {
        let mut graph = fixture();
        for preset_id in ["RenderMode", "SceneFog", "SceneLoop"] {
            let recipe = manifold_nodes::bundled_presets::bundled_preset_def(
                &manifold_core::PresetTypeId::new(preset_id),
            )
            .unwrap();
            let instance = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
                &graph,
                recipe.as_ref(),
                preset_id.into(),
                manifold_core::scene_modifier_preset::SceneNodeRef {
                    scope: vec![],
                    node: "scan_render".into(),
                },
                manifold_core::scene_modifier_preset::SceneTargetSelection::AllObjects,
            )
            .unwrap();
            graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
                &graph,
                graph.scene_modifiers.len(),
                instance,
            )
            .unwrap()
            .graph;
        }
        let mut gp = manifold_core::effects::PresetInstance::new_generator(
            manifold_core::PresetTypeId::new("PhotoscanBaseline"),
        );
        gp.graph = Some(graph.clone());
        gp.refresh_manifest_from_graph();
        let vm = manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(&graph).unwrap();
        let surfaces = super::modifier_surfaces(
            &gp,
            &graph,
            &vm,
            "layer",
            &[],
            (manifold_core::Bpm(120.0), 0.0),
        );
        let render_mode = surfaces.iter().find(|surface| surface.title == "Render Mode").unwrap();
        assert!(!render_mode.modifier.as_ref().unwrap().targets_all);
        assert!(render_mode.modifier.as_ref().unwrap().objects.is_empty());
        let scene_loop = surfaces.iter().find(|surface| surface.title == "Scene Loop").unwrap();
        assert!(scene_loop.modifier.as_ref().unwrap().targets_all);
        assert!(!scene_loop.modifier.as_ref().unwrap().objects.is_empty());
        assert!(scene_loop.rows.iter().any(|row| row.spec.name == "Camera Travel"));
    }
}
