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
    /// Every manifest param becomes a row. The Scene Setup dock's full
    /// generator surface uses this so scale/material rows reach the panel; the
    /// panel then filters by SECTION, not by `card_visible`.
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
        for card in ui.inspector.modifier_cards_mut() {
            crate::ui_translate::with_param_slots(&gp_state.params, |slots| {
                card.sync_values(tree, slots)
            });
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
    let Some(catalog_def) = manifold_renderer::node_graph::bundled_preset_def(inst.effect_type()) else {
        return (sp_def.default_value.to_string(), None);
    };
    let graph = inst.graph.as_ref().unwrap_or(catalog_def);
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
        relight: crate::ui_translate::relight_card_config_from(inst),
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
    // table marks `card_visible: false` (`scene_exposure::card_visible_for`) —
    // it stays a real, fully addressable manifest entry (OSC, Ableton, macros,
    // drivers all still resolve it by id), it just never becomes a CARD row.
    // `All` keeps every param (the Scene Setup dock's full generator surface).
    // The per-frame value push no longer mirrors this filter: `sync_card_values`
    // hands the FULL manifest as an id-keyed channel and the card JOINS by id
    // (BUG-313), so a hidden param simply finds no row — there is no second
    // filter to drift out of alignment.
    let visible_params: Vec<&manifold_core::params::Param> = match visibility {
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
            ParamRow {
                id: std::borrow::Cow::Owned(id),
                spec: RowSpec {
                    name: p.spec.name.clone(),
                    min: p.spec.min,
                    max: p.spec.max,
                    default: p.spec.default_value,
                    whole_numbers: p.spec.whole_numbers,
                    is_angle: p.spec.is_angle,
                    is_toggle: p.spec.is_toggle,
                    is_trigger: p.spec.is_trigger,
                    is_trigger_gate: p.spec.is_trigger_gate,
                    value_labels,
                    section: p.spec.section.clone(),
                    material_role: p.spec.material_role.map(super::material::role),
                    inactive_reason: None,
                    // The base projection leaves every row interactive;
                    // `modifier_surfaces` below re-locks Math View's
                    // Connect to Mesh when the chain doesn't support it.
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
                material_attached: false,
                audio: Default::default(),
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
        rows[pi].audio = audio_row_state(am);
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
                        ParamCardStringInfo {
                            name: sp_def.name.to_string(),
                            key: sp_def.key.to_string(),
                            value,
                            use_dropdown: sp_def.use_dropdown,
                            effect_id: None,
                            binding_id: None,
                            dropdown_choices: Vec::new(),
                            display_value: None,
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
        relight: crate::ui_translate::relight_card_config_from(inst),
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
    param_surface(
        gp,
        manifold_core::preset_def::PresetKind::Generator,
        0,
        OscScope::Layer(layer_id),
        clip_string_params,
        automation_latched,
        visibility,
        timing,
    )
    .expect("generator param_surface always yields a config")
}

/// The same disk/project catalog supplies picker entries and attached snapshots.
/// Full applicability is checked transactionally when the user applies a file.
pub(crate) fn modifier_picker_entries(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    vm: &manifold_renderer::node_graph::scene_vm::SceneVm,
) -> Vec<manifold_ui::param_surface::ModifierPickerEntry> {
    use manifold_renderer::preset_loader::SCENE_MODIFIER_CATALOG;
    use manifold_ui::param_surface::ModifierPickerEntry;
    let catalog = SCENE_MODIFIER_CATALOG.load();
    let mut entries: Vec<_> = catalog.entries().filter_map(|(id, json)| {
        if !catalog.is_browser_visible(&id) { return None; }
        let recipe: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(&json).ok()?;
        let metadata = recipe.preset_metadata.as_ref()?;
        if !metadata.available { return None; }
        let attachment = metadata.scene_modifier.as_ref()?;
        let disabled = if vm.multiple_scenes {
            Some("Select a graph with one scene".to_string())
        } else if attachment.singleton && def.scene_modifiers.iter().any(|instance|
            instance.graph.preset_metadata.as_ref().is_some_and(|m| m.id == metadata.id)) {
            Some("Already applied".to_string())
        } else { None };
        Some(ModifierPickerEntry { preset_id: id.to_string(), label: metadata.display_name.clone(), disabled })
    }).collect();
    entries.sort_by(|a, b| a.label.cmp(&b.label).then(a.preset_id.cmp(&b.preset_id)));
    entries
}

/// A card owns rows through stable host-to-instance bindings. Titles and
/// sections are presentation only; modulation still addresses the generator.
pub(crate) fn modifier_surfaces(
    gp: &manifold_core::effects::PresetInstance,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    _vm: &manifold_renderer::node_graph::scene_vm::SceneVm,
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
        let enabled = enabled_row.and_then(|row| {
            let param = gp.params.get(row.id.as_ref())?;
            let binding = bindings.iter().find(|binding| binding.id == row.id.as_ref())?;
            Some(manifold_core::effects::apply_card_reshape(param.base, param.spec.min, param.spec.max,
                param.spec.invert, param.spec.curve, binding.scale, binding.offset) > 0.5)
        }).unwrap_or(false);
        let legacy_scope = manifold_core::scene_modifier_math_view::has_legacy_scope_control(&instance.graph);
        let mut rows: Vec<_> = full.rows.iter().filter(|row| local_id(row.id.as_ref()).is_some_and(|id|
            id != recipe.enabled_param && !(legacy_scope && id == "math_view_scope")
                && !recipe.preparation_params.iter().any(|p| p == id))).cloned().collect();
        for row in &mut rows { row.scene_addr = None; }
        // Math View's Connect to Mesh locks when the static support check
        // fails (the compiler enforces the same rule at preparation — the
        // unsupported appearance simply never reaches the scene). The card
        // projects the reason onto the row so the toggle reads as locked
        // instead of looking live and silently doing nothing.
        if manifold_core::scene_modifier_math_view::is_math_view_recipe(&instance.graph) {
            let connect_mesh_id = format!(
                "{}connect_mesh",
                manifold_core::scene_modifier_math_view::CONTROL_PREFIX
            );
            let reason = manifold_core::scene_modifier_math_view::math_view_connect_support(
                def,
                &instance.id,
            )
            .err();
            for row in &mut rows {
                if local_id(row.id.as_ref()) == Some(connect_mesh_id.as_str()) {
                    row.spec.disabled = reason.clone();
                }
            }
        }
        Some(ParamSurface {
            kind: ParamCardKind::Effect,
            title: metadata.display_name.clone(),
            rows,
            string_params: vec![],
            audio_sends: Vec::new(),
            modifier: Some(ModifierCardInfo {
                instance_id: instance.id.clone(),
                layer_id: manifold_core::LayerId::new(layer_id),
                enabled_label: enabled_row.map(|row| row.spec.name.clone()).unwrap_or_else(|| "Enabled".into()),
                stack_index: index,
                stack_len: def.scene_modifiers.len(),
                targets_all: matches!(instance.targets, SceneTargetSelection::AllObjects),
                objects: manifold_renderer::node_graph::scene_modifier_authoring::scene_modifier_objects(def, &instance.scene)
                    .unwrap_or_else(|error| {
                        log::error!("scene modifier {} object selection unavailable: {error}", instance.id);
                        Vec::new()
                    }).into_iter().map(|object| {
                        let selected = match &instance.targets {
                            SceneTargetSelection::AllObjects => true,
                            SceneTargetSelection::Explicit { objects } => objects.contains(&object),
                        };
                        ModifierObjectOption {
                            label: modifier_object_label(def, &object),
                            object: ModifierObjectRef { scope: object.scope, node: object.node },
                            selected,
                        }
                    }).collect(),
            }),
            effect_index: 0,
            effect_id: manifold_core::EffectId::new(format!("scene_modifier:{}", instance.id)),
            enabled,
            collapsed: false,
            supports_envelopes: true,
            has_graph_mod: crate::modifier_preset::has_graph_mod(def, &instance.graph),
            layer_id: None,
            relight: crate::ui_translate::relight_card_config_from(gp),
        })
    }).collect()
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

/// Build a human-readable description for a macro mapping target.
pub(crate) fn describe_macro_mapping(
    target: &manifold_core::MacroMappingTarget,
    project: &manifold_core::project::Project,
) -> String {
    use manifold_core::MacroMappingTarget;
    match target {
        MacroMappingTarget::MasterOpacity => "Master Opacity".to_string(),
        MacroMappingTarget::Effect {
            effect_id,
            param_id,
        } => {
            let Some(fx) = project.find_effect_by_id(effect_id) else {
                return "Effect → ?".to_string();
            };
            let effect_type = fx.effect_type();
            // Effect display name is type-level template metadata (a boundary
            // read); the param name comes off the LIVE manifest so user-added /
            // glb params resolve instead of rendering "?" (was a registry
            // id-lookup miss, the UI twin of the P4 blind spot).
            let effect_name = manifold_core::preset_definition_registry::try_get(effect_type)
                .map(|d| d.display_name.clone())
                .unwrap_or_else(|| effect_type.as_str().to_string());
            let param_name = fx
                .params
                .get(param_id.as_ref())
                .map(|p| p.spec.name.clone())
                .unwrap_or_else(|| "?".to_string());
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
                    format!("{} {} → {}", layer_name, effect_name, param_name)
                }
                None => format!("{} → {}", effect_name, param_name),
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
            format!("{} Opacity", layer_name)
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
                .and_then(|gp| gp.params.get(param_id.as_ref()).map(|p| p.spec.name.clone()))
                .unwrap_or_else(|| "?".to_string());
            format!("{} Gen → {}", layer_name, param_name)
        }
    }
}

#[cfg(test)]
mod param_label_tests {
    use super::*;
    use manifold_core::MacroMappingTarget;
    use manifold_core::effects::PresetInstance;
    use manifold_core::params::{Param, ParamManifest};

    fn user_spec(id: &str, name: &str) -> manifold_core::effect_graph_def::ParamSpecDef {
        manifold_core::effect_graph_def::ParamSpecDef {
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
        let label = describe_macro_mapping(&target, &project);
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
        let mut graph = manifold_renderer::node_graph::bundled_preset_def(generator.effect_type())
            .expect("Oscilloscope generator preset is bundled")
            .clone();
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
mod modifier_audio_projection_tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    use manifold_renderer::node_graph::{scene_modifier_authoring::prepare_new_scene_modifier, scene_vm::SceneVm};

    #[test]
    fn modifier_surfaces_keep_audio_on_its_parameter_after_filtering_and_stack_reorder() {
        let mut graph: EffectGraphDef = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../manifold-renderer/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
        ))).unwrap();
        for preset in ["RenderMode", "SceneFog"] {
            let recipe = manifold_renderer::node_graph::bundled_preset_def(
                &manifold_core::PresetTypeId::new(preset),
            ).unwrap();
            let modifier = prepare_new_scene_modifier(
                &graph, recipe, preset.into(),
                SceneNodeRef { scope: vec![], node: "scan_render".into() },
                SceneTargetSelection::AllObjects,
            ).unwrap();
            graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
                &graph, graph.scene_modifiers.len(), modifier,
            ).unwrap().graph;
        }
        let mut gp = PresetInstance::new_generator(manifold_core::PresetTypeId::new("PhotoscanBaseline"));
        gp.graph = Some(graph.clone());
        gp.refresh_manifest_from_graph();
        // Match the reported collision: generator audio at full row 4,
        // while Render Mode's local row 4 is Line Color R.
        let prefix = (0..5).map(|i| {
            let mut spec = gp.params.iter().next().unwrap().spec.clone();
            spec.id = format!("host_{i}");
            Param::user_added(spec)
        });
        gp.params = ParamManifest::from_params(prefix.chain(gp.params.iter().cloned()).collect());
        let make_mod = |id: &str| ParameterAudioMod::new(
            id.to_string().into(), "host".into(),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        let mut host_mod = make_mod("host_4");
        host_mod.shape.release_ms = 63.616074;
        gp.audio_mods = Some(vec![host_mod]);
        // Hidden enable rows can have bindings too; their audio must never
        // leak into a visible parameter when the card drops them.
        for binding in &graph.preset_metadata.as_ref().unwrap().bindings {
            if let manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } = &binding.target
                && param_id == "enabled"
            {
                gp.audio_mods.as_mut().unwrap().push(make_mod(&binding.id));
            }
        }
        let vm = SceneVm::from_def(&graph).unwrap();
        let surfaces = modifier_surfaces(&gp, &graph, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
        assert_eq!(surfaces.len(), 2);
        assert_eq!(surfaces[0].rows[4].spec.name, "Line Color R");
        for surface in &surfaces {
            assert!(surface.rows.iter().all(|row| !row.audio.active),
                "generator audio must not appear on {}", surface.title);
        }
        let render_param = surfaces[0].rows[4].id.clone();
        let fog_param = surfaces[1].rows[0].id.clone();
        let mut disabled_mod = make_mod(&surfaces[0].rows[0].id);
        disabled_mod.enabled = false;
        gp.audio_mods.as_mut().unwrap().push(disabled_mod);
        let mut render_mod = make_mod(&render_param);
        render_mod.shape.release_ms = 250.0;
        render_mod.source.send_id = "render".into();
        render_mod.shape.range_min = 0.2;
        render_mod.shape.range_max = 0.8;
        let mut fog_mod = make_mod(&fog_param);
        fog_mod.source.send_id = "fog".into();
        fog_mod.shape.range_min = 0.35;
        fog_mod.shape.range_max = 0.65;
        gp.audio_mods.as_mut().unwrap().extend([render_mod, fog_mod]);
        let mut setup = manifold_core::audio_setup::AudioSetup::default();
        let mut host_send = manifold_core::audio_setup::AudioSend::new("Host");
        host_send.id = "host".into();
        let mut render_send = manifold_core::audio_setup::AudioSend::new("Render");
        render_send.id = "render".into();
        let mut fog_send = manifold_core::audio_setup::AudioSend::new("Fog");
        fog_send.id = "fog".into();
        setup.sends = vec![host_send, render_send, fog_send];
        for pass in 0..2 {
            let mut surfaces = modifier_surfaces(&gp, &graph, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
            attach_audio_sends(&mut surfaces, &setup);
            for (index, surface) in surfaces.iter().enumerate() {
                assert_eq!(surface.audio_sends.len(), 3);
                let send_order: Vec<&str> = surface.audio_sends.iter().map(|send| send.id.as_str()).collect();
                assert_eq!(send_order, if pass == 0 {
                    vec!["host", "render", "fog"]
                } else {
                    vec!["fog", "render", "host"]
                });
                let recipe = graph.scene_modifiers[index]
                    .graph
                    .preset_metadata
                    .as_ref()
                    .unwrap()
                    .scene_modifier
                    .as_ref()
                    .unwrap();
                assert!(surface.rows.iter().all(|row| {
                    let binding = graph.preset_metadata.as_ref().unwrap().bindings.iter()
                        .find(|binding| binding.id == row.id.as_ref()).unwrap();
                    let manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } = &binding.target
                        else { panic!("modifier row must have a modifier binding") };
                    param_id != &recipe.enabled_param
                        && !recipe.preparation_params.contains(param_id)
                }));
                for row in &surface.rows {
                    let audio = &row.audio;
                    assert_eq!(audio.active, row.id == render_param || row.id == fog_param);
                    if row.id == render_param {
                        assert_eq!(audio.release_ms, 250.0);
                        assert_eq!(audio.send_id.as_ref().map(|id| id.as_str()), Some("render"));
                        assert_eq!((audio.range_min, audio.range_max), (0.2, 0.8));
                    } else if row.id == fog_param {
                        assert_eq!(audio.send_id.as_ref().map(|id| id.as_str()), Some("fog"));
                        assert_eq!((audio.range_min, audio.range_max), (0.35, 0.65));
                    }
                }
            }
            if pass == 0 {
                setup.sends.reverse();
            }
            graph.scene_modifiers.reverse();
            gp.graph = Some(graph.clone());
        }
    }

    /// The Connect to Mesh row on a standalone Math View card carries the
    /// static support check's reason when no preceding patch-based modifier
    /// qualifies — the row must read as locked, every other row stays live.
    #[test]
    fn math_view_connect_mesh_row_locks_without_a_patch_carrier() {
        use manifold_core::effect_graph_def::SerializedParamValue;
        use manifold_core::scene_modifier_preset::{SceneMeshReferenceFrame, SceneModifierInstanceDef};

        let mut owner: EffectGraphDef = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../manifold-renderer/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
        ))).unwrap();
        owner.version = 3;
        let view_recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../manifold-renderer/assets/scene-modifier-presets/MathView.json"
        ))).unwrap();
        // One sampled object — the support check only reads frame targets.
        let container = owner.nodes.iter().find(|node| node.group.is_some()).unwrap();
        let group = container.group.as_ref().unwrap();
        let source = group.nodes.iter().find(|node| node.type_id == "node.cube_mesh").unwrap();
        let object = group.nodes.iter().find(|node| node.type_id == "node.scene_object").unwrap();
        let transform = group.nodes.iter().find(|node| node.type_id == "node.transform_3d").unwrap();
        let scope = vec![container.node_id.clone()];
        let frame = SceneMeshReferenceFrame {
            target: SceneNodeRef { scope: scope.clone(), node: object.node_id.clone() },
            source: SceneNodeRef { scope, node: source.node_id.clone() },
            source_definition_hash: manifold_core::scene_source_identity::scene_source_definition_hash(
                &owner,
                source,
            )
            .unwrap(),
            source_offset: ["pos_x", "pos_y", "pos_z"].map(|param| {
                match transform.params.get(param) {
                    Some(SerializedParamValue::Float { value }) => f64::from(*value),
                    _ => 0.0,
                }
            }),
            scene_radius: 3.0,
        };
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: "math_view".into(),
            scene: SceneNodeRef { scope: vec![], node: "scan_render".into() },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: vec![frame],
            legacy_math_view_carrier: None,
            graph: Box::new(view_recipe),
        });
        let owner = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
            &owner,
            &manifold_core::NodeId::new("math_view"),
        )
        .unwrap()
        .graph;

        let mut gp = PresetInstance::new_generator(manifold_core::PresetTypeId::new("PhotoscanBaseline"));
        gp.graph = Some(owner.clone());
        gp.refresh_manifest_from_graph();
        let vm = SceneVm::from_def(&owner).unwrap();
        let surfaces = modifier_surfaces(&gp, &owner, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
        assert_eq!(surfaces.len(), 1, "one Math View card");
        let surface = &surfaces[0];
        let bindings = owner.preset_metadata.as_ref().unwrap().bindings.as_slice();
        let local_of = |row: &manifold_ui::param_surface::ParamRow| {
            bindings.iter().find_map(|binding| {
                if binding.id != row.id.as_ref() {
                    return None;
                }
                match &binding.target {
                    manifold_core::effect_graph_def::BindingTarget::SceneModifier { modifier_id, param_id }
                        if modifier_id.as_str() == "math_view" => Some(param_id.as_str()),
                    _ => None,
                }
            })
        };
        let connect_row = surface
            .rows
            .iter()
            .find(|row| local_of(row) == Some("math_view_connect_mesh"))
            .expect("Connect to Mesh row projected");
        let reason = connect_row
            .spec
            .disabled
            .as_deref()
            .expect("unsupported chain must lock the row");
        assert!(reason.contains("patch-based"), "reason names the carrier requirement, got {reason:?}");
        assert!(
            surface
                .rows
                .iter()
                .filter(|row| local_of(row) != Some("math_view_connect_mesh"))
                .all(|row| row.spec.disabled.is_none()),
            "only the Connect to Mesh row locks"
        );
        let carrier: EffectGraphDef = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../manifold-core/tests/fixtures/math-view-legacy/vortex-fragments-initial-ce78a59d0.json"
        ))).unwrap();
        let mut migrated = owner.clone();
        manifold_core::scene_modifier_math_view::preserve_legacy_scope_control(
            &carrier, &mut migrated.scene_modifiers[0].graph);
        migrated = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
            &migrated, &manifold_core::NodeId::new("math_view")).unwrap().graph;
        let scope_id = migrated.preset_metadata.as_ref().unwrap().bindings.iter()
            .find(|binding| matches!(&binding.target,
                manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. }
                    if param_id == "math_view_scope")).unwrap().id.clone();
        gp.graph = Some(migrated.clone());
        gp.refresh_manifest_from_graph();
        assert!(gp.params.contains(&scope_id), "legacy animation still has a parameter target");
        let surfaces = modifier_surfaces(&gp, &migrated, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
        assert!(surfaces[0].rows.iter().all(|row| row.id.as_ref() != scope_id),
            "legacy Scope must not reappear on the standalone card");
    }
}

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
        }
    }

    fn tree_has_text(ui: &UIRoot, needle: &str) -> bool {
        ui.tree
            .nodes()
            .iter()
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
    #[test]
    fn modifier_picker_omits_retired_factory_combinations() {
        let def = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"),
            "/../manifold-renderer/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"))).unwrap();
        let vm = manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def).unwrap();
        let entries = super::modifier_picker_entries(&def, &vm);
        for id in ["SurfacePeel", "OrderedRecon", "SurfaceWaves", "SpatialEchoes"] {
            assert!(entries.iter().any(|entry| entry.preset_id == id), "missing {id}");
        }
        for id in ["SurfacePeelHit", "OrderedReconHit", "MaskedPeel", "WavesEchoes"] {
            assert!(!entries.iter().any(|entry| entry.preset_id == id), "retired {id} still in picker");
        }
    }
}
