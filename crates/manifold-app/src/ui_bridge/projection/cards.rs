//! Card-surface projection: THE `param_surface` manifest walk that builds
//! effect/generator `ParamSurface`s, its thin adapters and helpers, the
//! per-frame card VALUE sync, and the macro-mapping label. Moved from
//! state_sync.rs (P-P, UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_ui::panels::param_card::{ParamCardKind, ParamCardStringInfo, RowMod};
use manifold_ui::panels::param_slider_shared::AbletonMappingDisplay;
use manifold_ui::param_surface::{ParamRow, ParamSurface, RowMapping, RowSpec, RowValue};
use crate::ui_root::UIRoot;

use super::inspector::{build_audio_card_state, build_card_modulation};

/// OSC address scope for effect param configs.
/// Master effects use `/master/`, layer effects use `/layer/{id}/`, clips have no OSC.
#[derive(Clone, Copy)]
pub(crate) enum OscScope<'a> {
    Master,
    Layer(&'a str),
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
}

/// Stamp the card-level available-send list (labels + ids) onto every card
/// config, from the project's `AudioSetup`. One pass after the configs are
/// built, so the per-instance builders stay project-agnostic.
pub(crate) fn attach_audio_sends(configs: &mut [ParamSurface], setup: &manifold_core::audio_setup::AudioSetup) {
    if setup.sends.is_empty() {
        return;
    }
    let labels: Vec<String> = setup.sends.iter().map(|s| s.label.clone()).collect();
    let ids: Vec<manifold_core::AudioSendId> = setup.sends.iter().map(|s| s.id.clone()).collect();
    for c in configs.iter_mut() {
        c.audio.send_labels = labels.clone();
        c.audio.send_ids = ids.clone();
    }
}

/// Thin adapter: build a card config for each effect in `effects`, skipping
/// any whose preset def is missing. The real work is the unified
/// [`param_surface`].
pub(crate) fn effects_to_surfaces(
    effects: &[PresetInstance],
    osc_scope: OscScope<'_>,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
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
        rows: vec![],
        string_params: vec![],
        audio: Default::default(),
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
) -> Option<ParamSurface> {
    use manifold_core::preset_def::PresetKind;
    let preset_type = inst.effect_type();
    let reg_def = manifold_core::preset_definition_registry::try_get(preset_type);

    match kind {
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
    let row_index_of: ahash::AHashMap<String, usize> =
        inst.params.iter().enumerate().map(|(i, p)| (p.id().to_string(), i)).collect();

    let mut rows: Vec<ParamRow> = inst
        .params
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
                },
                // D7: display-value resolution decided here — base/effective
                // straight off the manifest slot, `driven` false (state_sync
                // has no wire-fed presentation case; only the editor snapshot
                // path sets it).
                value: RowValue { base: p.base, effective: p.value, exposed: p.exposed, driven: false },
                modulation: RowMod::default(),
                mapping: RowMapping { osc_address, ableton_display, ableton_range, mappable: true },
            }
        })
        .collect();
    let n = rows.len();

    let mod_rows = build_card_modulation(
        inst,
        n,
        |id| row_index_of.get(id).copied(),
        automation_latched,
    );
    for (row, rm) in rows.iter_mut().zip(mod_rows) {
        row.modulation = rm;
    }
    let audio = build_audio_card_state(inst, n, |id| row_index_of.get(id).copied());

    // String params are a generator-only surface (text inputs, font dropdowns),
    // sourced from the registry def.
    let string_params: Vec<ParamCardStringInfo> = match kind {
        PresetKind::Generator => reg_def
            .as_deref()
            .map(|def| {
                def.string_param_defs
                    .iter()
                    .map(|sp_def| {
                        let value = clip_string_params
                            .and_then(|m| m.get(sp_def.key))
                            .cloned()
                            .unwrap_or_else(|| sp_def.default_value.to_string());
                        ParamCardStringInfo {
                            name: sp_def.name.to_string(),
                            key: sp_def.key.to_string(),
                            value,
                            use_dropdown: sp_def.use_dropdown,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        PresetKind::Effect => Vec::new(),
    };

    let (card_kind, effect_id, enabled, collapsed, has_graph_mod) = match kind {
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
        rows,
        has_graph_mod,
        audio,
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
) -> ParamSurface {
    param_surface(
        gp,
        manifold_core::preset_def::PresetKind::Generator,
        0,
        OscScope::Layer(layer_id),
        clip_string_params,
        automation_latched,
    )
    .expect("generator param_surface always yields a config")
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
        sync_inspector_data(&mut ui, &project, None, &selection, &[]);
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
}
