//! Dropdown / picker builders: the `try_open_dropdown` intercept, the Add-picker
//! item construction, the Ableton picker session, and the audio-channel dropdown
//! builders. Moved verbatim from ui_root/mod.rs (UI_FUNNEL_DECOMPOSITION P-F2a,
//! pure move).

use super::*;
use manifold_core::LayerId;
use manifold_ui::panels::actions::AutomationShape;
use manifold_ui::{
    AudioSetupAction, BrowserAction, ClipAction, EditingAction, LayerAction, MappingAction,
    ParamsAction, ProjectAction, RootAction, TransportAction,
};

/// One preset menu vocabulary for all card families; addressing is captured by
/// the caller, independently of the current editor or later selection changes.
fn card_edit_menu_items(can_paste: bool, effects: bool, grouped: bool) -> Vec<DropdownItem> {
    use manifold_ui::panels::actions::CardEditAction::*;
    let mut entries = vec![
        ("Cut", Cut),
        ("Copy", Copy),
        ("Paste", Paste),
        ("Duplicate", Duplicate),
        ("Delete", Delete),
    ];
    if effects {
        entries.push(("Group", Group));
        if grouped {
            entries.push(("Ungroup", Ungroup));
        }
    }
    entries
        .into_iter()
        .map(|(label, edit)| {
            let mut item = DropdownItem::new(label)
                .with_action(PanelAction::Params(ParamsAction::EditCards(edit)));
            item.enabled = edit != Paste || can_paste;
            item.separator_after = edit == Delete;
            item
        })
        .collect()
}

fn preset_menu_items(
    modified: bool,
    action: impl Fn(manifold_ui::panels::actions::PresetActionKind) -> ParamsAction,
) -> Vec<DropdownItem> {
    use manifold_ui::panels::actions::PresetActionKind::*;
    let mut entries = vec![("Make Unique", MakeUnique)];
    if modified {
        entries.extend([
            ("Revert to Library", RevertToLibrary),
            (
                "Push to Library — updates instances tracking this preset",
                PushToLibrary,
            ),
        ]);
    }
    entries.extend([
        ("Save to Library…", SaveToLibrary),
        ("Save to Project…", SaveToProject),
        ("Export Preset…", Export),
        ("Import Preset…", Import),
    ]);
    entries
        .into_iter()
        .map(|(label, kind)| {
            DropdownItem::new(label).with_action(PanelAction::Params(action(kind)))
        })
        .collect()
}

fn automation_lane_context_items(
    target: &manifold_ui::view::UiGraphTarget,
    param_id: &manifold_core::effects::ParamId,
    layer_id: Option<LayerId>,
    beat: Option<manifold_core::Beats>,
    point_context: Option<(manifold_core::Beats, f32)>,
    pinned: bool,
    whole_numbers: bool,
) -> Vec<DropdownItem> {
    let mut items = Vec::new();
    items.push(
        DropdownItem::new(if pinned { "Unpin lane" } else { "Pin lane" }).with_action(
            PanelAction::Editing(EditingAction::AutomationLanePinToggle(
                target.clone(),
                param_id.clone(),
            )),
        ),
    );
    let hide_label = if pinned {
        "Hide lane (unpin first)"
    } else {
        "Hide lane"
    };
    items.push(if pinned {
        DropdownItem::disabled(hide_label)
    } else {
        DropdownItem::new(hide_label).with_action(PanelAction::Editing(
            EditingAction::AutomationLaneHide(target.clone(), param_id.clone()),
        ))
    });
    items.push(
        DropdownItem::new("Move lane up").with_action(PanelAction::Editing(
            EditingAction::AutomationLaneMove(target.clone(), param_id.clone(), -1),
        )),
    );
    items.push(
        DropdownItem::new("Move lane down").with_action(PanelAction::Editing(
            EditingAction::AutomationLaneMove(target.clone(), param_id.clone(), 1),
        )),
    );
    items.push(
        DropdownItem::new("Show all automated lanes")
            .with_separator()
            .with_action(PanelAction::Editing(EditingAction::ShowAllAutomationLanes)),
    );
    items.push(
        DropdownItem::new("Choose parameter…").with_action(PanelAction::Params(
            ParamsAction::OpenAutomationChooser(layer_id),
        )),
    );
    if let Some(beat) = beat {
        items.push(DropdownItem::new("Cut").with_action(PanelAction::Editing(
            EditingAction::ContextAutomationCut(target.clone(), param_id.clone(), beat),
        )));
        items.push(DropdownItem::new("Copy").with_action(PanelAction::Editing(
            EditingAction::ContextAutomationCopy(target.clone(), param_id.clone(), beat),
        )));
        items.push(
            DropdownItem::new("Paste here").with_action(PanelAction::Editing(
                EditingAction::ContextAutomationPaste(target.clone(), param_id.clone(), beat),
            )),
        );
        items.push(DropdownItem::new("Duplicate").with_separator().with_action(
            PanelAction::Editing(EditingAction::ContextAutomationDuplicate(
                target.clone(),
                param_id.clone(),
                beat,
            )),
        ));
    }
    items.push(
        DropdownItem::new("Select all points").with_action(PanelAction::Editing(
            EditingAction::ContextSelectAllAutomation(target.clone(), param_id.clone()),
        )),
    );
    items.push(
        DropdownItem::new("Delete selected points")
            .with_separator()
            .with_action(PanelAction::Editing(
                EditingAction::ContextDeleteSelectedAutomation(target.clone(), param_id.clone()),
            )),
    );
    if let Some(beat) = beat {
        items.push(
            DropdownItem::new("Insert shape…")
                .with_separator()
                .with_action(PanelAction::Editing(
                    EditingAction::ContextOpenAutomationShapePicker(
                        target.clone(),
                        param_id.clone(),
                        beat,
                    ),
                )),
        );
    }
    if let Some((beat, value_norm)) = point_context {
        for (label, shape) in [
            ("Straighten", manifold_ui::view::UiSegmentShape::Linear),
            ("Hold", manifold_ui::view::UiSegmentShape::Hold),
            ("Ease in", manifold_ui::view::UiSegmentShape::Curved(0.7)),
            ("Ease out", manifold_ui::view::UiSegmentShape::Curved(-0.7)),
        ] {
            if whole_numbers && shape != manifold_ui::view::UiSegmentShape::Hold {
                continue;
            }
            items.push(DropdownItem::new(label).with_action(PanelAction::Editing(
                EditingAction::ContextAutomationSetShape(
                    target.clone(),
                    param_id.clone(),
                    beat,
                    value_norm,
                    shape,
                ),
            )));
        }
        items.push(
            DropdownItem::new("Edit point value…").with_action(PanelAction::Editing(
                EditingAction::AutomationPointEditValue(
                    target.clone(),
                    param_id.clone(),
                    beat,
                    value_norm,
                ),
            )),
        );
        items.push(
            DropdownItem::new("Edit point time…")
                .with_separator()
                .with_action(PanelAction::Editing(
                    EditingAction::AutomationPointEditTime(
                        target.clone(),
                        param_id.clone(),
                        beat,
                        value_norm,
                    ),
                )),
        );
    }
    items.push(
        DropdownItem::new("Restore automation").with_action(PanelAction::Project(
            ProjectAction::ContextRestoreAutomationLane(target.clone(), param_id.clone()),
        )),
    );
    items.push(
        DropdownItem::new("Clear points").with_action(PanelAction::Project(
            ProjectAction::ContextClearAutomationLane(target.clone(), param_id.clone()),
        )),
    );
    items.push(
        DropdownItem::new("Clear Automation").with_action(PanelAction::Project(
            ProjectAction::ContextRemoveAutomationLane(target.clone(), param_id.clone()),
        )),
    );
    items
}

/// Shared mask sources for the card context menu and group modifier picker.
fn mask_menu_items(
    prefix: &str,
    layers: &[(manifold_core::LayerId, String)],
    action: impl Fn(String, Option<manifold_core::LayerId>) -> ParamsAction,
) -> Vec<DropdownItem> {
    let mut items = Vec::new();
    for (label, preset_id) in [
        ("Circle", "MaskCircle"),
        ("Rectangle", "MaskRectangle"),
        ("Gradient", "MaskGradient"),
        ("Image", "MaskImage"),
        ("Blob Detector", "MaskBlob"),
    ] {
        items.push(
            DropdownItem::new(&format!("{prefix} — {label}"))
                .with_action(PanelAction::Params(action(preset_id.to_string(), None))),
        );
    }
    if layers.is_empty() {
        items.push(DropdownItem::disabled(&format!(
            "{prefix} — Layer (no visual layers)"
        )));
    } else {
        for (layer_id, layer_name) in layers {
            items.push(
                DropdownItem::new(&format!("{prefix} — Layer: {layer_name}")).with_action(
                    PanelAction::Params(action("MaskLayer".to_string(), Some(layer_id.clone()))),
                ),
            );
        }
    }
    items
}

/// PRESET_BROWSER_AUDITION D8/§3.4 gate: may a preset whose metadata
/// carries `layer_types` appear on `invoking`? `None` on the preset = every
/// layer type; `Some(list)` = only the listed types. `None` as the invoking
/// type (effect mode) disables the gate — the picker then lists everything,
/// same as before the field existed.
fn preset_allowed_on_layer_type(
    layer_types: &Option<Vec<manifold_core::types::LayerType>>,
    invoking: Option<manifold_core::types::LayerType>,
) -> bool {
    match layer_types {
        None => true,
        Some(list) => invoking.is_some_and(|t| list.contains(&t)),
    }
}

/// Build the source assignment menu for one parameter. The target snapshot is
/// the authority for both the current source and the compatible lane ids, so
/// a stale or unavailable lane never appears as a selectable item.
fn trigger_source_menu_items(
    target: &crate::ui_bridge::TriggerTargetChoice,
    sources: &[crate::ui_bridge::TriggerSourceChoice],
) -> Vec<DropdownItem> {
    let mut items = Vec::new();
    if target.has_main_source {
        items.push(
            DropdownItem::new("Main lane")
                .with_check(target.source == manifold_ui::view::UiClipTriggerSource::Main)
                .with_action(PanelAction::Params(ParamsAction::AssignClipTriggerSource(
                    target.target.clone(),
                    target.param_id.clone(),
                    manifold_ui::view::UiClipTriggerSource::Main,
                ))),
        );
    }
    items.push(
        DropdownItem::new("No source")
            .with_check(
                target.source == manifold_ui::view::UiClipTriggerSource::Disabled
                    || (!target.has_main_source
                        && target.source == manifold_ui::view::UiClipTriggerSource::Main),
            )
            .with_action(PanelAction::Params(ParamsAction::AssignClipTriggerSource(
                target.target.clone(),
                target.param_id.clone(),
                manifold_ui::view::UiClipTriggerSource::Disabled,
            ))),
    );
    for source in sources
        .iter()
        .filter(|source| target.eligible_sources.contains(&source.id))
    {
        items.push(
            DropdownItem::new(&source.label)
                .with_check(
                    target.source
                        == manifold_ui::view::UiClipTriggerSource::Lane(source.id.clone()),
                )
                .with_action(PanelAction::Params(ParamsAction::AssignClipTriggerSource(
                    target.target.clone(),
                    target.param_id.clone(),
                    manifold_ui::view::UiClipTriggerSource::Lane(source.id.clone()),
                ))),
        );
    }
    items.push(
        DropdownItem::new("Create trigger lane")
            .with_separator()
            .with_action(PanelAction::Params(ParamsAction::CreateTriggerLane {
                owner: target.owner.clone(),
                assignment: Some((target.target.clone(), target.param_id.clone())),
            })),
    );
    items
}

struct TriggerTargetPicker {
    items: Vec<manifold_ui::panels::picker_core::PickerItem>,
    actions: Vec<PanelAction>,
    secondary_actions: Vec<Option<PanelAction>>,
    categories: Vec<String>,
}

/// Both buttons carry the manifest address; item IDs only retain picker focus.
fn trigger_target_picker_items(
    layer_id: &LayerId,
    targets: &[crate::ui_bridge::TriggerTargetChoice],
) -> TriggerTargetPicker {
    let mut picker = TriggerTargetPicker {
        items: Vec::new(),
        actions: Vec::new(),
        secondary_actions: Vec::new(),
        categories: Vec::new(),
    };
    for target in targets
        .iter()
        .filter(|target| target.eligible_sources.contains(layer_id))
    {
        let assigned =
            target.source == manifold_ui::view::UiClipTriggerSource::Lane(layer_id.clone());
        if !picker.categories.contains(&target.group_label) {
            picker.categories.push(target.group_label.clone());
        }
        picker
            .items
            .push(manifold_ui::panels::picker_core::PickerItem {
                type_id: format!("{:?}/{}", target.target, target.param_id),
                label: if assigned {
                    format!("✓ {}", target.label)
                } else {
                    target.label.clone()
                },
                category: Some(target.group_label.clone()),
                search_text: None,
                source: None,
                thumbnail: None,
            });
        let source = if assigned {
            manifold_ui::view::UiClipTriggerSource::Disabled
        } else {
            manifold_ui::view::UiClipTriggerSource::Lane(layer_id.clone())
        };
        picker
            .actions
            .push(PanelAction::Params(ParamsAction::AssignClipTriggerSource(
                target.target.clone(),
                target.param_id.clone(),
                source,
            )));
        picker.secondary_actions.push(Some(PanelAction::Params(
            ParamsAction::ShowClipTriggerResponse(target.target.clone(), target.param_id.clone()),
        )));
    }
    picker
}

#[cfg(test)]
mod tests {
    use super::preset_allowed_on_layer_type;
    use manifold_core::LayerId;
    use manifold_core::types::LayerType;
    use manifold_ui::panels::PanelAction;

    #[test]
    fn automation_numeric_menu_captures_the_clicked_point_value() {
        let target = manifold_ui::view::UiGraphTarget::Generator(LayerId::new("layer"));
        let items = super::automation_lane_context_items(
            &target,
            &"amount".into(),
            None,
            Some(manifold_core::Beats(6.0)),
            Some((manifold_core::Beats(4.0), 0.8)),
            false,
            false,
        );
        for (label, is_value) in [("Edit point value…", true), ("Edit point time…", false)] {
            let action = items
                .iter()
                .find(|item| item.label == label)
                .unwrap()
                .action
                .as_ref()
                .unwrap();
            let (actual_target, param, beat, value) = match action {
                PanelAction::Editing(manifold_ui::EditingAction::AutomationPointEditValue(
                    t,
                    p,
                    b,
                    v,
                )) if is_value => (t, p, b, v),
                PanelAction::Editing(manifold_ui::EditingAction::AutomationPointEditTime(
                    t,
                    p,
                    b,
                    v,
                )) if !is_value => (t, p, b, v),
                _ => panic!("numeric menu must capture point identity"),
            };
            assert_eq!(*actual_target, target);
            assert_eq!(param.as_ref(), "amount");
            assert_eq!(*beat, manifold_core::Beats(4.0));
            assert_eq!(*value, 0.8);
        }
        for label in ["Straighten", "Hold", "Ease in", "Ease out"] {
            assert!(
                items.iter().any(|item| item.label == label),
                "missing segment action: {label}"
            );
        }
        assert!(
            matches!(
                items.iter().find(|item| item.label == "Paste here").unwrap().action.as_ref(),
                Some(PanelAction::Editing(manifold_ui::EditingAction::ContextAutomationPaste(_, _, beat)))
                    if *beat == manifold_core::Beats(6.0)
            ),
            "pasting on a segment uses the clicked beat, independently of its left handle"
        );
    }

    /// LED presets (`layer_types: [Dmx]`) must be unreachable from a video /
    /// generator layer's picker and reachable from a DMX layer (§4
    /// invariant: "LED presets unreachable from video-layer pickers").
    #[test]
    fn led_presets_gate_to_dmx_layers_only() {
        let led = Some(vec![LayerType::Dmx]);
        assert!(
            preset_allowed_on_layer_type(&led, Some(LayerType::Dmx)),
            "DMX layer must see LED presets"
        );
        assert!(
            !preset_allowed_on_layer_type(&led, Some(LayerType::Generator)),
            "generator layer must NOT see LED presets"
        );
        assert!(
            !preset_allowed_on_layer_type(&led, Some(LayerType::Video)),
            "video layer must NOT see LED presets"
        );
        assert!(
            !preset_allowed_on_layer_type(&led, None),
            "effect mode (no invoking layer type) must NOT surface gated presets"
        );
    }

    /// A preset without `layer_types` is general: every layer type, both
    /// picker modes — the gate must not change the legacy item set.
    #[test]
    fn unscoped_presets_stay_visible_everywhere() {
        let general: Option<Vec<LayerType>> = None;
        for invoking in [
            None,
            Some(LayerType::Video),
            Some(LayerType::Generator),
            Some(LayerType::Dmx),
        ] {
            assert!(
                preset_allowed_on_layer_type(&general, invoking),
                "unscoped preset must appear for invoking {invoking:?}"
            );
        }
    }
}

/// Convert an AbletonSession into the picker's thin data struct.
pub(crate) fn build_picker_session(
    session: &AbletonSession,
) -> manifold_ui::panels::ableton_picker::AbletonPickerSession {
    use manifold_ui::panels::ableton_picker::{
        AbletonPickerSession, PickerDevice, PickerMacro, PickerTrack,
    };

    const RACK_CLASSES: &[&str] = &[
        "InstrumentGroupDevice",
        "DrumGroupDevice",
        "AudioEffectGroupDevice",
        "MidiEffectGroupDevice",
    ];

    let rack_tracks = session
        .tracks
        .iter()
        .filter_map(|track| {
            let devices: Vec<PickerDevice> = track
                .devices
                .iter()
                .filter(|d| RACK_CLASSES.contains(&d.class_name.as_str()) && !d.macros.is_empty())
                .map(|d| PickerDevice {
                    device_id: d.device_id,
                    device_name: d.name.clone(),
                    device_class_name: d.class_name.clone(),
                    macros: d
                        .macros
                        .iter()
                        .map(|m| PickerMacro {
                            param_id: m.param_id,
                            name: m.name.clone(),
                        })
                        .collect(),
                })
                .collect();
            if devices.is_empty() {
                None
            } else {
                Some(PickerTrack {
                    track_id: track.track_id,
                    track_name: track.name.clone(),
                    devices,
                })
            }
        })
        .collect();

    AbletonPickerSession { rack_tracks }
}

impl UIRoot {
    /// Open a dropdown whose items carry their own actions (2b.11). No
    /// `DropdownContext` is stored — each item returns
    /// `DropdownAction::SelectedAction`, which the drain fires directly, so there
    /// is no positional index→meaning map to keep in sync.
    pub(crate) fn open_dropdown_typed(&mut self, items: Vec<DropdownItem>, trigger: Rect) {
        self.dropdown_context = None;
        self.dropdown.open(items, trigger, 120.0, &mut self.tree);
        self.mark_overlay_dirty();
    }

    /// Open the searchable one-column list picker below `trigger`: typing
    /// filters, arrows move, Enter or a click fires the item's action.
    /// `actions` is parallel to `labels`.
    pub(crate) fn open_action_list(
        &mut self,
        labels: Vec<String>,
        actions: Vec<PanelAction>,
        list: manifold_ui::panels::browser_popup::ActionListOptions,
        trigger: Rect,
    ) {
        use manifold_ui::panels::browser_popup::*;
        use manifold_ui::panels::picker_core::PickerItem;
        let items = labels
            .into_iter()
            .map(|label| PickerItem {
                type_id: label.clone(),
                label,
                category: None,
                search_text: None,
                source: None,
                thumbnail: None,
            })
            .collect();
        self.browser_popup
            .set_screen_size(self.screen_width, self.screen_height);
        self.browser_popup.open_actions(
            BrowserPopupRequest {
                mode: BrowserPopupMode::Actions,
                tab: self.inspector.active_tab(),
                layer_id: None,
                items,
                category_names: Vec::new(),
                spawn_graph_pos: None,
                paste_count: 0,
                screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
            },
            actions,
            list,
        );
        self.mark_overlay_dirty();
    }

    /// Refresh the embedded-preset list surfaced into the Add pickers from the
    /// project. The frame planner runs it when the EmbeddedPresets stamp moved
    ///.
    pub fn sync_embedded_presets(&mut self, project: &manifold_core::project::Project) {
        manifold_core::work_count::record_work(
            manifold_core::work_count::WorkKind::PresetListProjection,
        );
        self.embedded_presets = project
            .embedded_presets
            .iter()
            .filter_map(|ep| {
                let meta = ep.def.preset_metadata.as_ref()?;
                Some(EmbeddedPresetItem {
                    kind: ep.kind,
                    type_id: meta.id.as_str().to_string(),
                    display_name: meta.display_name.to_string(),
                    origin: ep.origin,
                })
            })
            .collect();
    }

    /// D13 search haystack for one preset picker item — a PARTIAL seed: presets
    /// have no alias source in the registry (the node picker's descriptor
    /// aliases never extended to preset metadata), so the haystack carries what
    /// exists — the label, the category, and the id stem with camelCase and
    /// separators split into words — so category queries ("stylize") and
    /// id-shaped queries ("edge detect" for EdgeDetect) hit. Real aliases wait
    /// for an alias authoring source (design §7, Deferred). The haystack
    /// REPLACES the label in filtering (search_text.unwrap_or(label)), so the
    /// label is always first.
    fn preset_search_haystack(label: &str, category: Option<&str>, id: &str) -> String {
        let mut stem = String::with_capacity(id.len() + 8);
        let mut prev: Option<char> = None;
        for ch in id.chars() {
            if ch == '_' || ch == '-' {
                stem.push(' ');
            } else if ch.is_ascii_uppercase()
                && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
            {
                stem.push(' ');
                stem.push(ch);
            } else {
                stem.push(ch);
            }
            prev = Some(ch);
        }
        let mut hay = label.to_string();
        if let Some(c) = category {
            hay.push(' ');
            hay.push_str(c);
        }
        if stem.trim() != label {
            hay.push(' ');
            hay.push_str(stem.trim());
        }
        hay
    }

    /// Classify one `kind`'s Add-picker items by source
    /// — the single place this rule lives, so
    /// `AddEffectClicked` and `GenTypeClicked` can't drift apart:
    /// - **Factory** / **My Library**: every id `preset_type_registry`
    ///   resolves, split by `UserLibrary::is_user_entry` (a file under the
    ///   user root vs. not).
    /// - **This Project**: every `origin: Saved` embedded preset, always
    ///   listed; an `origin: Snapshot` embedded preset ONLY when its id
    ///   resolves nowhere in the registry (its library file is gone) —
    ///   marked "missing from library" rather than "Project" so the browser
    ///   reads as plumbing, not a real project preset.
    ///
    /// `tag_project_category` sets `category: Some("Project")` on the
    /// This-Project items (Effect mode's existing "Project" chip grouping).
    /// Generator mode passes `true`: generator
    /// items carry their registry category (e.g. the LED presets' `"LED"`)
    /// and the chip row renders in the generator browser too.
    ///
    /// Ordinary-source cells carry no exceptional marker; the source row
    /// already says where an item lives.
    ///
    /// `invoking_layer_type` gates by the preset's `layer_types` metadata
    /// (D8/§3.4): `Some([Dmx])` presets (the LED-*) are offered only on DMX
    /// layers; `None` on a preset means every layer type. Effect mode passes
    /// `None` — this design adds no DMX effect gate.
    fn build_preset_picker_items(
        &self,
        kind: manifold_core::preset_def::PresetKind,
        tag_project_category: bool,
        invoking_layer_type: Option<manifold_core::types::LayerType>,
    ) -> Vec<manifold_ui::panels::picker_core::PickerItem> {
        use manifold_core::preset_type_registry;
        use manifold_ui::panels::picker_core::{PickerItem, Source};

        let lib = crate::user_library::UserLibrary::new();
        let available: Vec<preset_type_registry::PresetTypeRegistration> =
            preset_type_registry::available_of_kind(kind)
                .into_iter()
                .filter(|reg| preset_allowed_on_layer_type(&reg.layer_types, invoking_layer_type))
                .collect();
        let mut items: Vec<PickerItem> = available
            .iter()
            .map(|reg| {
                let is_user = lib.is_user_entry(kind, &reg.id);
                let id = reg.id.as_str().to_string();
                // A My-Library entry's PNG
                // sits beside its JSON (`UserLibrary::thumbnail_path`); a
                // Factory entry's comes from the committed one-shot bin
                // output. `None` (no `Path::is_file` check needed further)
                // when the file simply doesn't exist yet — clean text
                // fallback, never a browse-time render.
                let thumbnail = if is_user {
                    let p = lib.thumbnail_path(kind, reg.id.as_str());
                    p.is_file().then(|| p.to_string_lossy().into_owned())
                } else {
                    manifold_compositor::preset_thumbnail::factory_thumbnail_path(
                        kind,
                        reg.id.as_str(),
                    )
                    .filter(|p| p.is_file())
                    .map(|p| p.to_string_lossy().into_owned())
                };
                PickerItem {
                    label: reg.display_name.to_string(),
                    type_id: id.clone(),
                    category: if tag_project_category {
                        reg.category.map(|c| c.to_string())
                    } else {
                        None
                    },
                    search_text: Some(Self::preset_search_haystack(
                        reg.display_name,
                        reg.category,
                        reg.id.as_str(),
                    )),
                    source: Some(if is_user {
                        Source::MyLibrary
                    } else {
                        Source::Factory
                    }),
                    thumbnail,
                }
            })
            .collect();

        for e in self.embedded_presets.iter().filter(|e| e.kind == kind) {
            use manifold_core::project::EmbeddedOrigin;
            // An unresolvable Snapshot (its
            // library file gone) is self-containment plumbing, not a
            // user-manageable choice — it never becomes a PickerItem (the
            // layers using it still show it on the layer card). A Snapshot
            // whose id resolves elsewhere is already represented via
            // `available` above.
            if e.origin == EmbeddedOrigin::Snapshot {
                continue;
            }
            items.push(PickerItem {
                label: e.display_name.clone(),
                type_id: e.type_id.clone(),
                category: if tag_project_category {
                    Some("Project".to_string())
                } else {
                    None
                },
                search_text: Some(Self::preset_search_haystack(
                    &e.display_name,
                    if tag_project_category {
                        Some("Project")
                    } else {
                        None
                    },
                    &e.type_id,
                )),
                source: Some(Source::Project),
                // This-Project entries never get a thumbnail (D7 only
                // covers Save to Library + the factory bin) — text fallback.
                thumbnail: None,
            });
        }

        items
    }

    /// If the action is a dropdown / context-menu / picker trigger, open the
    /// overlay anchored appropriately and return true (action consumed).
    /// Otherwise return false.
    ///
    /// Single source of truth for "an overlay just opened → mark `overlay_dirty`".
    /// Every open path (dropdowns, right-click context menus, the browser popup,
    /// the Ableton picker) flows through here, so flagging the dirty bit once on a
    /// `true` return guarantees the next build re-records the overlay into
    /// `overlay_draw` and it actually paints this interaction. The bare
    /// `open_context` arms used to forget this individually, which is exactly why
    /// right-click context menus were flaky: they drew only when some *unrelated*
    /// state change happened to trigger a rebuild that same frame.
    pub(crate) fn try_open_dropdown(
        &mut self,
        action: &PanelAction,
        click_node: Option<NodeId>,
    ) -> bool {
        let opened = self.try_open_dropdown_inner(action, click_node);
        if opened {
            self.mark_overlay_dirty();
        }
        opened
    }

    fn try_open_dropdown_inner(
        &mut self,
        action: &PanelAction,
        click_node: Option<NodeId>,
    ) -> bool {
        let right_click_pos = self.last_right_click_pos;
        let trigger = if let Some(node) = click_node {
            self.tree.get_bounds(node)
        } else {
            Rect::new(100.0, 100.0, 80.0, 24.0)
        };

        match action {
            PanelAction::Root(RootAction::OpenColourMenu {
                target,
                param_ids,
                value,
            }) => {
                // The hex item edits over the swatch; the grid writes the
                // picked colour as one RGB edit (`dropdown_color_to_action`).
                let items = vec![
                    DropdownItem::new("Hex\u{2026}").with_action(PanelAction::Root(
                        RootAction::BeginColourTextInput {
                            target: target.clone(),
                            param_ids: param_ids.clone(),
                            anchor: trigger,
                            value: *value,
                        },
                    )),
                ];
                self.dropdown_context = Some(DropdownContext::Colour {
                    target: target.clone(),
                    param_ids: param_ids.clone(),
                });
                self.dropdown.open_context_with_colors(
                    items,
                    manifold_ui::color::COLOR_GRID.to_vec(),
                    manifold_ui::color::COLOR_GRID_COLS,
                    Vec2::new(trigger.x, trigger.y + trigger.height),
                    &mut self.tree,
                );
                true
            }
            PanelAction::Layer(LayerAction::BlendModeClicked(idx)) => {
                use manifold_core::types::BlendMode;
                // Typed dropdown (2b.11): each item carries its own SetBlendMode
                // action, so selection fires it directly — no DropdownContext /
                // index→meaning map for blend modes.
                let items: Vec<DropdownItem> = BlendMode::ALL
                    .iter()
                    .map(|m| {
                        // The label is the display name; the action carries the
                        // Debug form, exactly as the old index→action map did
                        // (`format!("{:?}", BlendMode::from_index(i))`).
                        DropdownItem::new(m.display_name()).with_action(PanelAction::Layer(
                            LayerAction::SetBlendMode(idx.clone(), format!("{:?}", m)),
                        ))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Root(RootAction::OpenRtQualityTierDropdown {
                field,
                realtime,
                anchor,
            }) => {
                use manifold_core::settings::RtQualityTier;
                // Items are built from the panel's synced snapshot — selecting
                // one writes the whole settings struct with just this field
                // changed, so rapid successive edits never compose on a stale
                // base (the cycle-button multi-click bug class).
                let settings = self.rt_quality_panel.current_settings();
                let current = field.get(if *realtime {
                    &settings.realtime
                } else {
                    &settings.export
                });
                let items: Vec<DropdownItem> = RtQualityTier::ALL
                    .iter()
                    .map(|t| {
                        let mut new_settings = settings;
                        let col = if *realtime {
                            &mut new_settings.realtime
                        } else {
                            &mut new_settings.export
                        };
                        field.set(col, *t);
                        DropdownItem::new(t.label())
                            .with_check(*t == current)
                            .with_action(PanelAction::Project(ProjectAction::ChangeRtQuality(
                                new_settings,
                            )))
                    })
                    .collect();
                self.open_dropdown_typed(items, *anchor);
                true
            }
            PanelAction::Root(RootAction::OpenRtQualityResDropdown { realtime, anchor }) => {
                use manifold_core::settings::RtRayResolution;
                let settings = self.rt_quality_panel.current_settings();
                let current = if *realtime {
                    settings.realtime.ray_resolution
                } else {
                    settings.export.ray_resolution
                };
                let items: Vec<DropdownItem> = RtRayResolution::ALL
                    .iter()
                    .map(|r| {
                        let mut new_settings = settings;
                        let col = if *realtime {
                            &mut new_settings.realtime
                        } else {
                            &mut new_settings.export
                        };
                        col.ray_resolution = *r;
                        DropdownItem::new(r.label())
                            .with_check(*r == current)
                            .with_action(PanelAction::Project(ProjectAction::ChangeRtQuality(
                                new_settings,
                            )))
                    })
                    .collect();
                self.open_dropdown_typed(items, *anchor);
                true
            }
            PanelAction::Root(RootAction::OpenRtQualityDenoiseDropdown { realtime, anchor }) => {
                use manifold_core::settings::RtSpatialDenoise;
                let settings = self.rt_quality_panel.current_settings();
                let current = if *realtime {
                    settings.realtime.spatial_denoise
                } else {
                    settings.export.spatial_denoise
                };
                let items: Vec<DropdownItem> = RtSpatialDenoise::ALL
                    .iter()
                    .map(|d| {
                        let mut new_settings = settings;
                        let col = if *realtime {
                            &mut new_settings.realtime
                        } else {
                            &mut new_settings.export
                        };
                        col.spatial_denoise = *d;
                        DropdownItem::new(d.label())
                            .with_check(*d == current)
                            .with_action(PanelAction::Project(ProjectAction::ChangeRtQuality(
                                new_settings,
                            )))
                    })
                    .collect();
                self.open_dropdown_typed(items, *anchor);
                true
            }
            PanelAction::Clip(ClipAction::ClipDetectQuantizeClicked) => {
                // Typed (2b.11): each grid option carries its quantize step.
                let items: Vec<DropdownItem> =
                    manifold_core::audio_clip_detection::quantize_grid_options()
                        .iter()
                        .map(|(label, step)| {
                            DropdownItem::new(label).with_action(PanelAction::Clip(
                                ClipAction::ClipDetectSetQuantize(*step),
                            ))
                        })
                        .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Clip(ClipAction::ClipDetectLayerClicked(idx)) => {
                // "Auto" (route by trigger name) first, then every candidate
                // layer cached by state_sync — each carries its target layer.
                let mut items = Vec::with_capacity(self.clip_detect_layers.len() + 1);
                items.push(DropdownItem::new("Auto").with_action(PanelAction::Clip(
                    ClipAction::ClipDetectSetLayer(*idx, None),
                )));
                for (id, name) in &self.clip_detect_layers {
                    items.push(DropdownItem::new(name).with_action(PanelAction::Clip(
                        ClipAction::ClipDetectSetLayer(*idx, Some(id.clone())),
                    )));
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            // The Audio Setup Triggers matrix's target-layer dropdown
            // (`AudioTriggerLayerClicked` → `AudioTriggerSetLayer`) is deleted
            // with the matrix (P3, D2).
            PanelAction::Root(RootAction::AudioSendClicked(idx)) => {
                // "No source" first, then every named send from Audio Setup so the
                // layer dropdown and the setup panel can never disagree — each
                // carries its SetLayerAudioSend directly.
                let sends = self.audio_setup_panel.send_options();
                let mut items = Vec::with_capacity(sends.len() + 1);
                items.push(
                    DropdownItem::new("No source").with_action(PanelAction::Layer(
                        LayerAction::SetLayerAudioSend(idx.clone(), None),
                    )),
                );
                for (id, label) in sends {
                    items.push(DropdownItem::new(&label).with_action(PanelAction::Layer(
                        LayerAction::SetLayerAudioSend(idx.clone(), Some(id)),
                    )));
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Params(ParamsAction::OpenAutomationChooser(layer_id)) => {
                use manifold_ui::panels::browser_popup::*;

                // The projection has already walked the selected layer's
                // manifests. Clone the bounded picker payload once at open;
                // the popup itself only filters its shared PickerCore and
                // dispatches the typed action captured beside each item.
                let candidates = self
                    .automation_chooser_candidates
                    .iter()
                    .filter(|candidate| {
                        layer_id.as_ref().is_none_or(|id| id == &candidate.layer_id)
                    });
                let mut items = Vec::new();
                let mut actions = Vec::new();
                let mut categories = Vec::new();
                for candidate in candidates {
                    if let Some(category) = candidate.item.category.as_deref()
                        && !categories.iter().any(|name| name == category)
                    {
                        categories.push(category.to_string());
                    }
                    items.push(candidate.item.clone());
                    actions.push(candidate.action.clone());
                }
                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open_actions(
                    BrowserPopupRequest {
                        mode: BrowserPopupMode::Actions,
                        tab: self.inspector.active_tab(),
                        layer_id: layer_id.clone(),
                        items,
                        category_names: categories,
                        spawn_graph_pos: None,
                        paste_count: 0,
                        screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                    },
                    actions,
                    ActionListOptions {
                        empty_label: "No parameters match",
                        ..ActionListOptions::default()
                    },
                );
                true
            }
            PanelAction::Params(ParamsAction::OpenClipTriggerSource(target, param_id)) => {
                let Some(target_choice) = self.trigger_routing.targets.iter().find(|candidate| {
                    candidate.target == *target && candidate.param_id == *param_id
                }) else {
                    return true;
                };
                let items = trigger_source_menu_items(target_choice, &self.trigger_routing.sources);
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Params(ParamsAction::OpenTriggerTargets(layer_id)) => {
                use manifold_ui::panels::browser_popup::*;
                let picker = trigger_target_picker_items(layer_id, &self.trigger_routing.targets);
                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open_actions(
                    BrowserPopupRequest {
                        mode: BrowserPopupMode::Actions,
                        tab: self.inspector.active_tab(),
                        layer_id: Some(layer_id.clone()),
                        items: picker.items,
                        category_names: picker.categories,
                        spawn_graph_pos: None,
                        paste_count: 0,
                        screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                    },
                    picker.actions,
                    ActionListOptions {
                        empty_label: "No compatible trigger targets",
                        secondary_actions: picker.secondary_actions,
                        keep_open: true,
                        ..Default::default()
                    },
                );
                self.mark_overlay_dirty();
                true
            }
            PanelAction::Params(ParamsAction::AddEffectClicked { tab, layer_id }) => {
                use manifold_core::{preset_def::PresetKind, preset_type_registry};
                use manifold_ui::panels::browser_popup::*;

                // Effect mode keeps its existing "Project" category chip
                // grouping (`tag_project_category: true`). No layer-type
                // gate in effect mode (D8 scopes the gate to generators).
                let mut items = self.build_preset_picker_items(PresetKind::Effect, true, None);
                let has_project_items = items
                    .iter()
                    .any(|it| it.category.as_deref() == Some("Project"));
                items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));

                // The atomic invocation context (D2): the action carries
                // exactly what the button rendered from — Master → no
                // layer; the Layer button → the inspected layer's id. The
                // request's `layer_id` slot (already used by generator
                // mode) carries it through the popup session to the pick;
                // dispatch builds EffectTarget from it, never re-resolving
                // the active layer.
                let (tab, layer_id) = (*tab, layer_id.clone());

                // Category chips — only buckets that actually hold items
                // (D9: Diagnostic left the filter row with the recuration;
                // an empty chip would filter the grid straight into the
                // empty state), plus "Project" when embedded effects exist.
                let mut cat_names: Vec<String> = preset_type_registry::ALL_CATEGORIES
                    .iter()
                    .filter(|c| items.iter().any(|it| it.category.as_deref() == Some(c)))
                    .map(|&c| c.to_string())
                    .collect();
                if has_project_items {
                    cat_names.push("Project".to_string());
                }

                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Effect,
                    tab,
                    layer_id,
                    items,
                    category_names: cat_names,
                    spawn_graph_pos: None,
                    paste_count: self.effect_clipboard_count,
                    screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                });
                true
            }
            PanelAction::Params(ParamsAction::GenTypeClicked(layer_id)) => {
                use manifold_core::preset_def::PresetKind;
                use manifold_ui::panels::browser_popup::*;

                // Generator items carry their
                // registry category and the browser renders the chip row —
                // same machinery as Effect mode, no fork.
                //
                // D8/§3.4: the invoking layer's type gates `layer_types`
                // presets — DMX lanes see the LED-* set, video/generator
                // lanes never do.
                let invoking_layer_type = self
                    .layer_headers
                    .layers()
                    .iter()
                    .find(|l| Some(l.layer_id.as_str()) == layer_id.as_ref().map(|id| id.as_str()))
                    .map(|l| {
                        if l.is_led_layer {
                            manifold_core::types::LayerType::Dmx
                        } else {
                            manifold_core::types::LayerType::Generator
                        }
                    });
                let mut items = self.build_preset_picker_items(
                    PresetKind::Generator,
                    true,
                    invoking_layer_type,
                );
                items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));

                // Category chips derived from the items themselves (first
                // appearance order after the label sort) — generator mode has
                // no fixed bucket list the way ALL_CATEGORIES is for effects;
                // a "Project" chip falls out of the same derivation when
                // embedded generator presets exist.
                let mut cat_names: Vec<String> = Vec::new();
                for it in items.iter() {
                    if let Some(c) = it.category.as_deref()
                        && !cat_names.iter().any(|n| n == c)
                    {
                        cat_names.push(c.to_string());
                    }
                }

                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Generator,
                    tab: InspectorTab::Layer,
                    layer_id: layer_id.clone(),
                    items,
                    category_names: cat_names,
                    spawn_graph_pos: None,
                    paste_count: 0,
                    screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                });

                // Lane-scoped open (MVP-P3c): from a DMX lane the browser
                // starts on the "LED" chip — a starting filter the performer
                // widens to "All", never a hard filter (D17: data on the
                // shared surface). AFTER open() on purpose: set_category
                // no-ops while the popup session is None, and the session is
                // created inside open(). The layer's type comes from the
                // layer-header snapshot state_sync feeds every frame — the
                // project reach UiRoot already has; no new channel, no
                // request field.
                let is_dmx_lane = layer_id.as_ref().is_some_and(|id| {
                    self.layer_headers
                        .layers()
                        .iter()
                        .any(|l| l.layer_id == id.as_str() && l.is_led_layer)
                });
                if is_dmx_lane {
                    self.browser_popup.set_category(Some("LED".to_string()));
                }
                true
            }
            PanelAction::Browser(BrowserAction::BrowserCellRightClicked(mode, type_id, source)) => {
                use manifold_ui::panels::picker_core::Source;

                let mut items = Vec::new();
                items.push(
                    DropdownItem::new("Rename…").with_action(PanelAction::Browser(
                        BrowserAction::BrowserRenamePresetClicked(*mode, type_id.clone(), *source),
                    )),
                );
                if matches!(source, Source::MyLibrary) {
                    items.push(
                        DropdownItem::new("Duplicate").with_action(PanelAction::Browser(
                            BrowserAction::BrowserDuplicatePresetClicked(*mode, type_id.clone()),
                        )),
                    );
                }
                items.push(
                    DropdownItem::new("Delete…").with_action(PanelAction::Browser(
                        BrowserAction::BrowserDeletePresetClicked(*mode, type_id.clone(), *source),
                    )),
                );
                if matches!(source, Source::MyLibrary) {
                    items.push(DropdownItem::new("Reveal in Finder").with_action(
                        PanelAction::Browser(BrowserAction::BrowserRevealPresetClicked(
                            *mode,
                            type_id.clone(),
                        )),
                    ));
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Project(ProjectAction::SelectAudioInputDevice) => {
                // Enumerate audio input devices on demand; each item carries its
                // SetAudioInputDevice action ("" = none/video-only).
                let device_names: Vec<String> =
                    manifold_audio::capture::AudioCaptureDevice::list_devices()
                        .into_iter()
                        .map(|d| d.name)
                        .collect();
                let mut items: Vec<DropdownItem> = vec![
                    DropdownItem::new("None (video only)").with_action(PanelAction::Project(
                        ProjectAction::SetAudioInputDevice(String::new()),
                    )),
                ];
                items.extend(device_names.into_iter().map(|name| {
                    DropdownItem::new(&name).with_action(PanelAction::Project(
                        ProjectAction::SetAudioInputDevice(name),
                    ))
                }));
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Root(RootAction::AudioSetupDeviceClicked) => {
                // Enumerate input devices + tappable sources on demand for the
                // Audio Setup modal. The list is three sections: the default, the
                // hardware/virtual input devices, and the output taps (system +
                // running apps). A parallel choice map records what each row is so
                // selection doesn't depend on position.
                manifold_core::work_count::record_work(
                    manifold_core::work_count::WorkKind::DeviceQuery,
                );
                let dir = manifold_audio::directory::system_directory();
                self.audio_setup_devices = dir.list_input_devices();
                let caps = dir.tap_capabilities();
                self.audio_setup_apps = if caps.app_audio {
                    dir.list_audio_apps()
                } else {
                    Vec::new()
                };

                // Typed (2b.11): each source row carries its AudioSetDevice action
                // built from the cached metadata; headers stay non-selectable.
                let mut items: Vec<DropdownItem> = Vec::new();

                // No capture at all — sends are fed by timeline audio layers
                // only, the device stays dark.
                items.push(DropdownItem::new("None (layers only)").with_action(
                    PanelAction::AudioSetup(AudioSetupAction::AudioSetDevice(Some(
                        manifold_ui::AudioDeviceRef::none(),
                    ))),
                ));

                items.push(DropdownItem::new("System Default").with_action(
                    PanelAction::AudioSetup(AudioSetupAction::AudioSetDevice(None)),
                ));

                if !self.audio_setup_devices.is_empty() {
                    items.push(DropdownItem::disabled("Input Devices"));
                    for d in &self.audio_setup_devices {
                        // Mark an offline device so a stale routing reads clearly.
                        let label = if d.is_alive {
                            d.name.clone()
                        } else {
                            format!("{} (offline)", d.name)
                        };
                        // Store stable UID + display name from the cached metadata.
                        let action =
                            PanelAction::AudioSetup(AudioSetupAction::AudioSetDevice(Some(
                                manifold_ui::AudioDeviceRef::new(d.uid.clone(), d.name.clone()),
                            )));
                        items.push(DropdownItem::new(&label).with_action(action));
                    }
                }

                if caps.system_audio || caps.app_audio {
                    items.push(DropdownItem::disabled("Capture Output"));
                    if caps.system_audio {
                        items.push(DropdownItem::new("System Audio").with_action(
                            PanelAction::AudioSetup(AudioSetupAction::AudioSetDevice(Some(
                                manifold_ui::AudioDeviceRef::system_audio(),
                            ))),
                        ));
                    }
                    for app in &self.audio_setup_apps {
                        // A backgrounded/idle app is still selectable; it just
                        // produces silence until it plays. Persist the stable bundle
                        // id + display name; the runtime re-resolves at capture time.
                        let label = if app.is_alive {
                            app.name.clone()
                        } else {
                            format!("{} (idle)", app.name)
                        };
                        let action = PanelAction::AudioSetup(AudioSetupAction::AudioSetDevice(
                            Some(manifold_ui::AudioDeviceRef::app(
                                app.bundle_id.clone(),
                                app.name.clone(),
                            )),
                        ));
                        items.push(DropdownItem::new(&label).with_action(action));
                    }
                }

                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Transport(TransportAction::SelectClkDevice) => {
                if self.midi_device_names.is_empty() {
                    log::info!("[UIRoot] No MIDI devices available for CLK selection");
                    return false;
                }
                // Typed (2b.11): item i carries SetMidiClockDevice(i).
                let items: Vec<DropdownItem> = self
                    .midi_device_names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        DropdownItem::new(name).with_action(PanelAction::Transport(
                            TransportAction::SetMidiClockDevice(i as i32),
                        ))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Layer(LayerAction::MidiInputClicked(idx)) => {
                // Typed dropdown (2b.11): item n carries SetMidiNote(idx, n).
                let items: Vec<DropdownItem> = (0..128)
                    .map(|n| {
                        DropdownItem::new(&manifold_core::midi::note_number_to_name(n)).with_action(
                            PanelAction::Project(ProjectAction::SetMidiNote(idx.clone(), n)),
                        )
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Layer(LayerAction::MidiChannelClicked(idx)) => {
                // "All" (-1) then channels 0..15 (displayed 1..16).
                let mut items: Vec<DropdownItem> = vec![DropdownItem::new("All").with_action(
                    PanelAction::Project(ProjectAction::SetMidiChannel(idx.clone(), -1)),
                )];
                items.extend((1..=16).map(|ch| {
                    DropdownItem::new(&format!("Ch {}", ch)).with_action(PanelAction::Project(
                        ProjectAction::SetMidiChannel(idx.clone(), ch - 1),
                    ))
                }));
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Layer(LayerAction::MidiDeviceClicked(idx)) => {
                // "All Devices" (None) then each named device.
                let mut items: Vec<DropdownItem> = vec![
                    DropdownItem::new("All Devices").with_action(PanelAction::Project(
                        ProjectAction::SetMidiDevice(idx.clone(), None),
                    )),
                ];
                items.extend(self.midi_device_names.iter().map(|name| {
                    DropdownItem::new(name).with_action(PanelAction::Project(
                        ProjectAction::SetMidiDevice(idx.clone(), Some(name.clone())),
                    ))
                }));
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Transport(TransportAction::ResolutionClicked) => {
                use manifold_core::types::ResolutionPreset;
                let has_displays = !self.display_resolutions.is_empty();

                // Typed dropdown (2b.11): each item carries its own action.
                let mut items: Vec<DropdownItem> = ResolutionPreset::ALL
                    .iter()
                    .map(|r| {
                        let (w, h) = r.dimensions();
                        DropdownItem::new(&r.dropdown_label())
                            .with_action(PanelAction::Project(ProjectAction::SetOutputSize(w, h)))
                    })
                    .collect();

                // Add display resolutions below presets (Unity: Footer.CollectDisplayResolutions)
                if has_displays {
                    // Separator label (disabled, non-selectable) — matches Unity format
                    items.push(DropdownItem::disabled("---  Displays  ---"));
                    for (w, h, label) in &self.display_resolutions {
                        items.push(
                            DropdownItem::new(&format!("{}  ({}x{})", label, w, h)).with_action(
                                PanelAction::Project(ProjectAction::SetOutputSize(
                                    *w as i32, *h as i32,
                                )),
                            ),
                        );
                    }
                }
                items.push(
                    DropdownItem::new("Custom\u{2026}").with_action(PanelAction::Transport(
                        TransportAction::CustomResolutionClicked,
                    )),
                );

                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Params(ParamsAction::MasterExitPathClicked) => {
                // Typed (2b.11): "After All FX" → exit -1, "Before FX" → 0, then each
                // effect → its 1-based exit index.
                let mut items = vec![
                    DropdownItem::new("After All FX")
                        .with_action(PanelAction::Params(ParamsAction::SetLedExitIndex(-1))),
                    DropdownItem::new("Before FX")
                        .with_action(PanelAction::Params(ParamsAction::SetLedExitIndex(0))),
                ];
                for (e, name) in self.master_effect_names.iter().enumerate() {
                    items.push(DropdownItem::new(&format!("After {}", name)).with_action(
                        PanelAction::Params(ParamsAction::SetLedExitIndex(e as i32 + 1)),
                    ));
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Editing(EditingAction::ClipRightClicked(clip_id)) => {
                // Typed (2b.11): each item carries its clip action.
                let items = vec![
                    DropdownItem::new("Split at Playhead").with_action(PanelAction::Editing(
                        EditingAction::ContextSplitAtPlayhead(clip_id.clone()),
                    )),
                    DropdownItem::new("Delete").with_action(PanelAction::Editing(
                        EditingAction::ContextDeleteClip(clip_id.clone()),
                    )),
                    DropdownItem::new("Duplicate").with_action(PanelAction::Editing(
                        EditingAction::ContextDuplicateClip(clip_id.clone()),
                    )),
                ];
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Editing(EditingAction::TrackRightClicked(beat, layer)) => {
                // Typed (2b.11): each item carries its track action. Paste stays
                // index-based (`ContextPasteAtTrack` targets the clicked track
                // slot, not a specific layer's identity) but the Context*Layer
                // family is LayerId-keyed (BUG-031) — resolve the row-under-
                // cursor's id once, synchronously, same as the layer-header menu.
                let mut items = vec![DropdownItem::new("Paste").with_action(PanelAction::Editing(
                    EditingAction::ContextPasteAtTrack(*beat, *layer),
                ))];
                if let Some(layer_id) = self
                    .layer_headers
                    .layer_info(*layer)
                    .map(|info| manifold_core::LayerId::new(&info.layer_id))
                {
                    let info = self.layer_headers.layer_info(*layer);
                    let is_trigger = info.is_some_and(|info| info.is_trigger);
                    // A trigger lane adds its sibling under the same parent.
                    let lane_owner = if is_trigger {
                        info.and_then(|i| i.parent_layer_id.as_deref())
                            .map(manifold_core::LayerId::new)
                    } else {
                        items.push(DropdownItem::new("Import MIDI File").with_action(
                            PanelAction::Editing(EditingAction::ContextImportMidi(
                                layer_id.clone(),
                            )),
                        ));
                        Some(layer_id.clone())
                    };
                    if let Some(owner) = lane_owner {
                        items.push(DropdownItem::new("Add trigger lane").with_action(
                            PanelAction::Params(ParamsAction::CreateTriggerLane {
                                owner,
                                assignment: None,
                            }),
                        ));
                    }
                    items.push(DropdownItem::new("Insert Video Layer").with_action(
                        PanelAction::Editing(EditingAction::ContextAddVideoLayer(layer_id.clone())),
                    ));
                    items.push(DropdownItem::new("Insert Generator Layer").with_action(
                        PanelAction::Editing(EditingAction::ContextAddGeneratorLayer(
                            layer_id.clone(),
                        )),
                    ));
                    items.push(DropdownItem::new("Insert LED Layer").with_action(
                        PanelAction::Editing(EditingAction::ContextAddLedLayer(layer_id.clone())),
                    ));
                    items.push(DropdownItem::new("Insert Audio Layer").with_action(
                        PanelAction::Editing(EditingAction::ContextAddAudioLayer(layer_id)),
                    ));
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Editing(EditingAction::AutomationLaneRightClicked(target, param_id)) => {
                use manifold_ui::automation_hit_tester::{AutomationHit, hit_test_automation};
                let lanes = self.viewport.automation_lane_screens();
                let hit = hit_test_automation(right_click_pos, lanes);
                let point = match hit {
                    Some(AutomationHit::Dot {
                        lane_index,
                        dot_index,
                    }) => Some(lanes[lane_index].dots[dot_index]),
                    Some(AutomationHit::Segment {
                        lane_index,
                        left_dot_index,
                    }) => Some(lanes[lane_index].dots[left_dot_index]),
                    _ => None,
                };
                let beat = match hit {
                    Some(AutomationHit::Dot {
                        lane_index,
                        dot_index,
                    }) => lanes[lane_index].dots[dot_index].beat,
                    _ => self
                        .viewport
                        .snap_to_grid(self.viewport.pixel_to_beat(right_click_pos.x))
                        .max(manifold_core::Beats::ZERO),
                };
                let layer_id = self
                    .automation_chooser_candidates
                    .iter()
                    .find_map(|candidate| {
                        let PanelAction::Params(ParamsAction::ShowAutomationAddress(
                            candidate_target,
                            _,
                        )) = &candidate.action
                        else {
                            return None;
                        };
                        (candidate_target == target).then(|| candidate.layer_id.clone())
                    });
                let pinned = self
                    .pinned_automation_lanes
                    .iter()
                    .any(|key| key.0 == *target && key.1 == *param_id);
                let whole_numbers = lanes.iter().any(|lane| {
                    lane.target == *target && lane.param_id == *param_id && lane.whole_numbers
                });
                let items = automation_lane_context_items(
                    target,
                    param_id,
                    layer_id,
                    Some(beat),
                    point.map(|dot| (dot.beat, dot.value_norm)),
                    pinned,
                    whole_numbers,
                );
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Editing(EditingAction::ContextOpenAutomationShapePicker(
                target,
                param_id,
                beat,
            )) => {
                let items = [
                    ("Ramp up", AutomationShape::RampUp),
                    ("Ramp down", AutomationShape::RampDown),
                    ("Triangle", AutomationShape::Triangle),
                    ("Sine", AutomationShape::Sine),
                    ("Square", AutomationShape::Square),
                    ("Hold low", AutomationShape::HoldLow),
                    ("Hold high", AutomationShape::HoldHigh),
                ]
                .into_iter()
                .map(|(label, shape)| {
                    DropdownItem::new(label).with_action(PanelAction::Editing(
                        EditingAction::ContextInsertAutomationShape(
                            target.clone(),
                            param_id.clone(),
                            *beat,
                            shape,
                        ),
                    ))
                })
                .collect();
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Editing(EditingAction::LayerHeaderRightClicked(layer_id)) => {
                // The action carries a stable LayerId; every context-menu item
                // below carries that same id (not a resolved row index), so a
                // layer-list change between menu-open and item-click can't make
                // an item address the wrong layer — see BUG-031. `li` is used
                // only for the synchronous, read-only display decisions below
                // (is_group / can_group), which are inherently open-time snapshots.
                let Some(li) = self.layer_headers.index_of_layer(layer_id) else {
                    return true;
                };
                let layer_info = self.layer_headers.layer_info(li);
                let is_group = layer_info.is_some_and(|l| l.is_group);
                let is_trigger = layer_info.is_some_and(|l| l.is_trigger);
                let mut items = vec![DropdownItem::new("Paste").with_action(PanelAction::Editing(
                    EditingAction::ContextPasteAtLayer(layer_id.clone()),
                ))];
                if !is_trigger {
                    items.push(DropdownItem::new("Add/show automation lane…").with_action(
                        PanelAction::Params(ParamsAction::OpenAutomationChooser(Some(
                            layer_id.clone(),
                        ))),
                    ));
                }
                if !is_group && !is_trigger {
                    items.push(DropdownItem::new("Import MIDI File").with_action(
                        PanelAction::Editing(EditingAction::ContextImportMidi(layer_id.clone())),
                    ));
                }
                // A trigger lane adds its sibling under the same parent.
                let lane_owner = if is_trigger {
                    layer_info
                        .and_then(|i| i.parent_layer_id.as_deref())
                        .map(manifold_core::LayerId::new)
                } else {
                    Some(layer_id.clone())
                };
                if let Some(owner) = lane_owner {
                    items.push(DropdownItem::new("Add trigger lane").with_action(
                        PanelAction::Params(ParamsAction::CreateTriggerLane {
                            owner,
                            assignment: None,
                        }),
                    ));
                }
                items.push(DropdownItem::new("Insert Video Layer").with_action(
                    PanelAction::Editing(EditingAction::ContextAddVideoLayer(layer_id.clone())),
                ));
                items.push(DropdownItem::new("Insert Generator Layer").with_action(
                    PanelAction::Editing(EditingAction::ContextAddGeneratorLayer(layer_id.clone())),
                ));
                items.push(DropdownItem::new("Insert LED Layer").with_action(
                    PanelAction::Editing(EditingAction::ContextAddLedLayer(layer_id.clone())),
                ));
                items.push(DropdownItem::new("Insert Audio Layer").with_action(
                    PanelAction::Editing(EditingAction::ContextAddAudioLayer(layer_id.clone())),
                ));
                items.push(
                    DropdownItem::new("Duplicate Layer").with_action(PanelAction::Editing(
                        EditingAction::ContextDuplicateLayer(layer_id.clone()),
                    )),
                );
                // "Group" only when 2+ non-group, non-nested layers are selected
                let can_group = self.layer_headers.layer_count() >= 2 && !is_group;
                if can_group {
                    items.push(DropdownItem::new("Group Selected Layers").with_action(
                        PanelAction::Editing(EditingAction::ContextGroupSelectedLayers),
                    ));
                }
                if is_group {
                    items.push(
                        DropdownItem::new("Ungroup").with_action(PanelAction::Editing(
                            EditingAction::ContextUngroup(layer_id.clone()),
                        )),
                    );
                }
                // Only allow delete if more than 1 layer exists
                if self.layer_headers.layer_count() > 1 {
                    items.push(DropdownItem::new("Delete Layer").with_action(
                        PanelAction::Editing(EditingAction::ContextDeleteLayer(layer_id.clone())),
                    ));
                }
                // Last text item gets a separator before the color grid
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                self.dropdown_context = Some(DropdownContext::LayerContext(layer_id.clone()));
                self.dropdown.open_context_with_colors(
                    items,
                    manifold_ui::color::COLOR_GRID.to_vec(),
                    manifold_ui::color::COLOR_GRID_COLS,
                    right_click_pos,
                    &mut self.tree,
                );
                true
            }
            PanelAction::Params(ParamsAction::ParamLabelRightClick(gpt, param_id)) => {
                let mut items = Vec::with_capacity(manifold_core::MACRO_COUNT + 6);
                // The inspector shows one tab at a time, so the tab says which
                // surface the row was right-clicked on.
                let pin_surface = if self.inspector.active_tab() == manifold_ui::InspectorTab::Scene
                {
                    match gpt {
                        GraphParamTarget::GeneratorOf(layer)
                            if self.scene_setup_panel.scene_layer_with(param_id) == Some(layer) =>
                        {
                            let on_card = self
                                .inspector
                                .scene_card_row(layer, param_id.as_ref())
                                .is_some();
                            Some(super::card_pin::PinSurface::SceneTab { on_card })
                        }
                        _ => None,
                    }
                } else if matches!(gpt, GraphParamTarget::Generator) {
                    self.inspector
                        .scene_card_layer()
                        .and_then(|layer| self.inspector.scene_card_row(layer, param_id.as_ref()))
                        .map(super::card_pin::PinSurface::Card)
                } else {
                    None
                };
                if let Some(surface) = pin_surface {
                    items.extend(super::card_pin::scene_items(gpt, param_id, surface));
                }
                let range_anchor =
                    manifold_ui::Rect::new(right_click_pos.x, right_click_pos.y, 140.0, 20.0);
                items.extend(super::card_pin::slider_range_items(
                    gpt,
                    param_id,
                    range_anchor,
                ));
                if self.inspector.active_tab().is_layer_scope()
                    || matches!(gpt, GraphParamTarget::GeneratorOf(_))
                {
                    items.push(DropdownItem::new("Show Automation").with_action(
                        PanelAction::Params(ParamsAction::ShowAutomation(
                            gpt.clone(),
                            param_id.clone(),
                        )),
                    ));
                    items.push(
                        DropdownItem::new("Clear Automation")
                            .with_action(PanelAction::Params(ParamsAction::ClearAutomation(
                                gpt.clone(),
                                param_id.clone(),
                            )))
                            .with_separator(),
                    );
                }
                for i in 0..manifold_core::MACRO_COUNT {
                    let label = {
                        let slot = &self.macro_labels[i];
                        if slot.is_empty() {
                            format!("Map to Macro {}", i + 1)
                        } else {
                            format!("Map to Macro {} ({})", i + 1, slot)
                        }
                    };
                    // Typed (2b.11): item i maps the param to macro i.
                    items.push(DropdownItem::new(&label).with_action(PanelAction::Mapping(
                        MappingAction::MapParamToMacro(gpt.clone(), param_id.clone(), i),
                    )));
                }
                // Ableton picker entry
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                let ableton_connected = self.ableton_session.as_ref().is_some_and(|s| s.connected);
                if ableton_connected {
                    items.push(DropdownItem::new("Map to Ableton Macro…").with_action(
                        PanelAction::Root(RootAction::OpenAbletonPickerForParam(
                            gpt.clone(),
                            param_id.clone(),
                        )),
                    ));
                } else {
                    items.push(DropdownItem::disabled("Ableton not connected"));
                }
                // "Remove Ableton Mapping" when param is already mapped — the
                // only kind-specific read; the menu + context are unified.
                let is_ableton_mapped = match gpt {
                    GraphParamTarget::Effect(effect_id) => self
                        .inspector
                        .is_effect_ableton_mapped(effect_id, param_id.as_ref()),
                    // `is_gen_ableton_mapped` reads the inspector's own
                    // generator card, which shows the Scene tab's layer too.
                    GraphParamTarget::Generator | GraphParamTarget::GeneratorOf(_) => {
                        self.inspector.is_gen_ableton_mapped(param_id.as_ref())
                    }
                };
                if is_ableton_mapped {
                    items.push(DropdownItem::new("Remove Ableton Mapping").with_action(
                        PanelAction::Mapping(MappingAction::UnmapParamAbleton(
                            gpt.clone(),
                            param_id.clone(),
                        )),
                    ));
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Mapping(MappingAction::MacroLabelRightClick(macro_idx)) => {
                // Typed (2b.11): rename, each mapping (unmap), Clear All, and the
                // Ableton entries each carry their own action.
                let descs = &self.macro_mapping_descs[*macro_idx];
                let rename = DropdownItem::new("Rename")
                    .with_action(PanelAction::Params(ParamsAction::MacroLabelRename(
                        *macro_idx,
                    )))
                    .with_separator();
                let mut items = vec![rename];
                if descs.is_empty() {
                    let mut item = DropdownItem::new("No mappings");
                    item.enabled = false;
                    items.push(item);
                } else {
                    for (i, desc) in descs.iter().enumerate() {
                        items.push(DropdownItem::new(desc).with_action(PanelAction::Mapping(
                            MappingAction::UnmapMacro(*macro_idx, i),
                        )));
                    }
                    if descs.len() > 1 {
                        if let Some(last) = items.last_mut() {
                            last.separator_after = true;
                        }
                        items.push(DropdownItem::new("Clear All").with_action(
                            PanelAction::Mapping(MappingAction::ClearMacroMappings(*macro_idx)),
                        ));
                    }
                }
                // Ableton section — same pattern as effect/gen param dropdowns
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                if self.ableton_session.is_some() {
                    items.push(
                        DropdownItem::new("Map to Ableton Macro\u{2026}").with_action(
                            PanelAction::Mapping(MappingAction::OpenAbletonPickerForMacro(
                                *macro_idx,
                            )),
                        ),
                    );
                } else {
                    let mut item = DropdownItem::new("Ableton not connected");
                    item.enabled = false;
                    items.push(item);
                }
                // "Remove Ableton Mapping" if this macro is mapped
                if self.macro_ableton_mapped[*macro_idx] {
                    items.push(DropdownItem::new("Remove Ableton Mapping").with_action(
                        PanelAction::Mapping(MappingAction::UnmapMacroAbleton(*macro_idx)),
                    ));
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Root(RootAction::SceneItemRightClicked) => {
                use manifold_ui::panels::actions::CardEditAction;
                self.object_cards_have_focus = true;
                let mut items =
                    card_edit_menu_items(self.scene_item_clipboard.is_some(), false, false);
                if let Some(selected) = self.scene_setup_panel.selected_scene_item() {
                    let allowed = |action| self.scene_setup_panel.scene_item_edit_allowed(action);
                    let (can_copy, can_cut, can_duplicate) = (
                        allowed(CardEditAction::Copy),
                        allowed(CardEditAction::Cut),
                        allowed(CardEditAction::Duplicate),
                    );
                    items.retain(|item| match item.label.as_str() {
                        "Copy" => can_copy,
                        "Cut" => can_cut,
                        "Delete" => selected.removable,
                        "Duplicate" => can_duplicate,
                        _ => true,
                    });
                    if let Some(group) = self.scene_setup_panel.group_selection_action() {
                        items.push(
                            DropdownItem::new("Group")
                                .with_action(group)
                                .with_separator(),
                        );
                    }
                    if let Some(ungroup) = self.scene_setup_panel.ungroup_selection_action() {
                        items.push(DropdownItem::new("Ungroup").with_action(ungroup));
                    }
                    items.push(
                        DropdownItem::new("Rename")
                            .with_action(PanelAction::Root(RootAction::SceneItemRename)),
                    );
                    items.push(
                        DropdownItem::new("Move Up")
                            .with_action(PanelAction::Root(RootAction::SceneItemMove(-1))),
                    );
                    items.push(
                        DropdownItem::new("Move Down")
                            .with_action(PanelAction::Root(RootAction::SceneItemMove(1))),
                    );
                    if !matches!(
                        selected.kind,
                        manifold_core::scene_surface::ItemKind::World
                            | manifold_core::scene_surface::ItemKind::Material
                            | manifold_core::scene_surface::ItemKind::Camera
                    ) {
                        items.push(
                            DropdownItem::new("Frame Selected")
                                .with_action(PanelAction::Root(RootAction::SceneItemFrame)),
                        );
                    }
                } else {
                    // Camera and World remain addressable for navigation; only paste applies.
                    items.retain(|item| item.label.starts_with("Paste"));
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Params(ParamsAction::EffectGroupRightClicked(group_id)) => {
                self.object_cards_have_focus = false;
                self.inspector.select_rack_group(group_id, false);
                self.inspector.apply_selection_visuals(&mut self.tree);
                let items = card_edit_menu_items(self.effect_clipboard.has_content(), true, true);
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Params(ParamsAction::EffectGroupAddModifierClicked(group_id)) => {
                let mut items = mask_menu_items(
                    "Mask",
                    &self.clip_detect_layers,
                    |preset_id, source_layer| ParamsAction::AddEffectGroupMask {
                        group_id: group_id.clone(),
                        preset_id,
                        source_layer,
                    },
                );
                if self.inspector.rack_group_has_mask(group_id) {
                    if let Some(last) = items.last_mut() {
                        last.separator_after = true;
                    }
                    items.push(
                        DropdownItem::new("Remove Mask").with_action(PanelAction::Params(
                            ParamsAction::RemoveEffectGroupMask(group_id.clone()),
                        )),
                    );
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Params(ParamsAction::CardRightClicked(gpt)) => {
                self.object_cards_have_focus = false;
                // Generators carry Copy/Paste (their own clipboard); both kinds
                // share Make Unique / Export / Import. The menu CONTENTS differ
                // per kind by design (the legitimately-divergent shell); the
                // fork actions + their dispatch are one path keyed by `gpt`.
                // Typed (2b.11): each item carries its action keyed by the card's
                // target, so the dispatch runs one path for effects + generators.
                let mut items = Vec::new();
                if let GraphParamTarget::Effect(clicked_id) = gpt {
                    self.inspector.select_effect_for_context_menu(clicked_id);
                    self.inspector.apply_selection_visuals(&mut self.tree);
                    items.extend(card_edit_menu_items(
                        self.effect_clipboard.has_content(),
                        true,
                        self.inspector.selection_has_group(),
                    ));
                    let selected = self.inspector.get_selected_effect_indices();
                    // A stale id (the effect left this chain) offers no mask action.
                    if let Some(idx) = self
                        .inspector
                        .effect_position(self.inspector.active_tab(), clicked_id)
                    {
                        let selected_indices = if selected.contains(&idx) {
                            selected
                        } else {
                            vec![idx]
                        };
                        items.extend(mask_menu_items(
                            "Add Mask",
                            &self.clip_detect_layers,
                            |preset_id, source_layer| ParamsAction::AddMask {
                                target: gpt.clone(),
                                selected_indices: selected_indices.clone(),
                                preset_id,
                                source_layer,
                            },
                        ));
                    }
                }
                if matches!(gpt, GraphParamTarget::Generator) {
                    items.push(
                        DropdownItem::new("Copy Generator")
                            .with_action(PanelAction::Params(ParamsAction::CopyGenerator)),
                    );
                    if self.gen_clipboard.has_content() {
                        items.push(
                            DropdownItem::new("Paste Generator")
                                .with_action(PanelAction::Params(ParamsAction::PasteGenerator)),
                        );
                    }
                }
                let has_graph_mod = match gpt {
                    GraphParamTarget::Effect(effect_id) => {
                        self.inspector.effect_has_graph_mod(effect_id)
                    }
                    // Same rationale as `is_ableton_mapped` above: card-only
                    // state, `GeneratorOf` doesn't reach `CardRightClicked` in
                    // practice, same query applies if it ever does.
                    GraphParamTarget::Generator | GraphParamTarget::GeneratorOf(_) => {
                        self.inspector.gen_has_graph_mod()
                    }
                };
                if self.inspector.card_has_sections(gpt) {
                    if let Some(last) = items.last_mut() {
                        last.separator_after = true;
                    }
                    for (label, folded) in [
                        ("Collapse all sections", true),
                        ("Expand all sections", false),
                    ] {
                        items.push(DropdownItem::new(label).with_action(PanelAction::Params(
                            ParamsAction::SetCardSectionsFolded(gpt.clone(), folded),
                        )));
                    }
                }
                items.extend(preset_menu_items(has_graph_mod, |kind| {
                    use manifold_ui::panels::actions::PresetActionKind::*;
                    match kind {
                        MakeUnique => ParamsAction::MakePresetUnique(gpt.clone()),
                        RevertToLibrary => ParamsAction::RevertToLibrary(gpt.clone()),
                        PushToLibrary => ParamsAction::PushToLibrary(gpt.clone()),
                        SaveToLibrary => ParamsAction::SaveToLibrary(gpt.clone()),
                        SaveToProject => ParamsAction::SaveToProject(gpt.clone()),
                        Export => ParamsAction::ExportPreset(gpt.clone()),
                        Import => ParamsAction::ImportPreset(gpt.clone()),
                    }
                }));
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::Root(RootAction::OpenAbletonPickerForParam(gpt, param_id)) => {
                use manifold_ui::panels::ableton_picker::AbletonPickerContext;
                if let Some(session) = &self.ableton_session {
                    // Carry the unified target straight through. The mapping
                    // target + inspector tab are resolved at dispatch time, the
                    // same path the unmap action uses — no kind fork here.
                    self.ableton_picker_context = Some(AbletonPickerContext::Param {
                        gpt: gpt.clone(),
                        param_id: param_id.clone(),
                    });
                    self.ableton_picker
                        .open(build_picker_session(session), right_click_pos);
                    self.mark_overlay_dirty();
                    self.ableton_rediscovery_needed = true;
                }
                true
            }
            PanelAction::Mapping(MappingAction::OpenAbletonPickerForMacro(slot_idx)) => {
                use manifold_ui::panels::ableton_picker::AbletonPickerContext;
                if let Some(session) = &self.ableton_session {
                    self.ableton_picker_context = Some(AbletonPickerContext::MacroSlot {
                        slot_idx: *slot_idx,
                    });
                    self.ableton_picker
                        .open(build_picker_session(session), right_click_pos);
                    self.mark_overlay_dirty();
                    self.ableton_rediscovery_needed = true;
                }
                true
            }
            // The shared card row core's enum value-cell click
            // (3+ labels) — same overlay as `SceneSetupEnumClicked`, but
            // generic over `GraphParamTarget` + `ParamId` so inspector card
            // rows and scene rows share the one path. Each item dispatches
            // `ParamEnumSet` (the Snapshot/Changed/Commit trio in one
            // action — one undo unit, no new mutation path).
            PanelAction::Root(RootAction::ParamEnumDropdown {
                target,
                param_id,
                labels,
                current_index,
                cell_node_id,
            }) => {
                let cell_trigger = self.tree.get_bounds(*cell_node_id);
                // An empty label is a value with nothing to pick (a deleted
                // camera's cut number), so it is not offered.
                let items: Vec<DropdownItem> = labels
                    .iter()
                    .enumerate()
                    .filter(|(_, label)| !label.is_empty())
                    .map(|(i, label)| {
                        DropdownItem::new(label)
                            .with_check(i as u32 == *current_index)
                            .with_action(PanelAction::Params(ParamsAction::ParamEnumSet(
                                target.clone(),
                                param_id.clone(),
                                i as f32,
                            )))
                    })
                    .collect();
                self.open_dropdown_typed(items, cell_trigger);
                true
            }
            // The Scene tab's menus (Add, field picks, Material features):
            // each item carries the action it sends.
            PanelAction::Root(RootAction::SceneSetupActionMenuClicked {
                button_node_id,
                items,
            }) => {
                let button = self.tree.get_bounds(*button_node_id);
                self.open_dropdown_typed(items.clone(), button);
                true
            }
            _ => false,
        }
    }

    /// Convert a color swatch selection into the actions it fires.
    pub(crate) fn dropdown_color_to_action(
        &self,
        ctx: DropdownContext,
        color_idx: usize,
    ) -> Vec<PanelAction> {
        let Some(color) = manifold_ui::color::COLOR_GRID.get(color_idx) else {
            return Vec::new();
        };
        match ctx {
            DropdownContext::LayerContext(layer_id) => {
                vec![PanelAction::Editing(EditingAction::ContextSetLayerColor(
                    layer_id, *color,
                ))]
            }
            // The same Begin, Move, Commit transaction the hex entry commits:
            // one undoable write to all three channels.
            DropdownContext::Colour { target, param_ids } => {
                crate::text_input::material_colour_scrub_actions(
                    crate::text_input::MaterialColourCtx { target, param_ids },
                    [color.r, color.g, color.b].map(|channel| f32::from(channel) / 255.0),
                )
                .into()
            }
        }
    }
}

#[cfg(test)]
mod led_browser_scoped_open_tests {
    //! Asserted against the real `UIRoot` the way
    //! the region structural tests do: a Generator-mode picker request built
    //! for a DMX-layer context opens the one shared browser with the "LED"
    //! chip active and only LED-category items filtered in; a plain
    //! generator lane opens unscoped ("All").

    use super::*;
    use manifold_ui::node::Color32;
    use manifold_ui::panels::layer_header::LayerInfo;
    use manifold_ui::view::{UiClipTriggerSource, UiGraphTarget};

    fn layer_info(name: &str, is_led_layer: bool) -> LayerInfo {
        LayerInfo {
            name: name.into(),
            layer_id: name.into(),
            is_collapsed: false,
            is_group: false,
            is_generator: true,
            is_audio: false,
            is_trigger: false,
            trigger_targets_label: String::new(),
            is_muted: false,
            is_solo: false,
            analysis_only: false,
            is_led: is_led_layer,
            is_led_layer,
            parent_layer_id: None,
            blend_mode: "Normal".into(),
            generator_type: None,
            clip_count: 0,
            video_folder_path: None,
            source_clip_count: 0,
            midi_note: -1,
            midi_channel: -1,
            midi_device: None,
            midi_all_notes: false,
            audio_gain_db: 0.0,
            audio_send_name: None,
            is_selected: false,
            color: Color32::new(100, 148, 210, 220),
        }
    }

    fn open_gen_browser(ui: &mut UIRoot, layer_id: Option<manifold_core::LayerId>) -> bool {
        ui.try_open_dropdown(
            &PanelAction::Params(ParamsAction::GenTypeClicked(layer_id)),
            None,
        )
    }

    #[test]
    fn gen_browser_from_dmx_lane_opens_scoped_to_led() {
        let mut ui = UIRoot::new();
        ui.resize(1536.0, 1216.0);
        ui.layer_headers.set_layers(vec![layer_info("LED 1", true)]);

        assert!(
            open_gen_browser(&mut ui, Some(manifold_core::LayerId::new("LED 1"))),
            "GenTypeClicked must open the browser popup"
        );

        let picker = ui
            .browser_popup
            .picker()
            .expect("browser session must exist after open()");

        // Items carry their registry category and the chip set derived from
        // them contains "LED" (steps 1-3 end to end, real bundled presets).
        assert!(
            picker
                .all_items()
                .any(|it| it.category.as_deref() == Some("LED")),
            "LED presets must reach the picker items with their category"
        );
        assert!(
            picker.categories().iter().any(|c| c == "LED"),
            "category_names derived from the items must contain \"LED\""
        );

        // Scoped open (step 5): the LED chip is active and only LED items
        // pass the filter — the starting chip the performer can widen.
        assert_eq!(picker.active_category(), Some("LED"));
        let filtered: Vec<&str> = picker.filtered().map(|(_, it)| it.label.as_str()).collect();
        assert!(
            !filtered.is_empty(),
            "scoped open must leave the ten LED presets visible"
        );
        assert_eq!(
            filtered.len(),
            picker
                .all_items()
                .filter(|it| it.category.as_deref() == Some("LED"))
                .count(),
            "every filtered item must be an LED-category item"
        );
    }

    #[test]
    fn gen_browser_from_plain_generator_lane_opens_unscoped() {
        let mut ui = UIRoot::new();
        ui.resize(1536.0, 1216.0);
        ui.layer_headers
            .set_layers(vec![layer_info("Generator 1", false)]);

        assert!(open_gen_browser(
            &mut ui,
            Some(manifold_core::LayerId::new("Generator 1"))
        ));

        let picker = ui
            .browser_popup
            .picker()
            .expect("browser session must exist after open()");
        assert_eq!(
            picker.active_category(),
            None,
            "a non-DMX lane must open on \"All\", not a scoped chip"
        );
        assert!(
            picker.filtered_len()
                > picker
                    .all_items()
                    .filter(|it| it.category.as_deref() == Some("LED"))
                    .count(),
            "unscoped open must list more than just the LED presets"
        );
    }

    #[test]
    fn trigger_source_menu_keeps_only_eligible_lanes_and_captures_assignment() {
        use crate::ui_bridge::{TriggerSourceChoice, TriggerTargetChoice};

        let target = TriggerTargetChoice {
            owner: LayerId::new("owner"),
            target: UiGraphTarget::Generator(LayerId::new("owner")),
            param_id: "fire".into(),
            label: "Owner / Fire".into(),
            group_label: "Owner".into(),
            has_main_source: true,
            source: UiClipTriggerSource::Main,
            eligible_sources: vec![LayerId::new("lane-a")],
        };
        let items = super::trigger_source_menu_items(
            &target,
            &[
                TriggerSourceChoice {
                    id: LayerId::new("lane-a"),
                    label: "Lane A".into(),
                },
                TriggerSourceChoice {
                    id: LayerId::new("lane-b"),
                    label: "Lane B".into(),
                },
            ],
        );
        assert!(items.iter().any(|item| item.label == "Lane A"));
        assert!(!items.iter().any(|item| item.label == "Lane B"));
        assert!(items.iter().any(|item| item.label == "Create trigger lane"));
        assert!(items.iter().any(|item| matches!(item.action.as_ref(),
            Some(PanelAction::Params(manifold_ui::ParamsAction::AssignClipTriggerSource(
                target @ UiGraphTarget::Generator(id), param, UiClipTriggerSource::Lane(lane)
            ))) if *target == UiGraphTarget::Generator(LayerId::new("owner"))
                && id == &LayerId::new("owner")
                && param.as_ref() == "fire"
                && lane == &LayerId::new("lane-a")
        )));
    }

    #[test]
    fn trigger_target_picker_marks_assigned_targets_and_toggles_them_off() {
        use crate::ui_bridge::TriggerTargetChoice;

        let target = TriggerTargetChoice {
            owner: LayerId::new("owner"),
            target: UiGraphTarget::Generator(LayerId::new("owner")),
            param_id: "fire".into(),
            label: "Owner / Fire".into(),
            group_label: "Owner".into(),
            has_main_source: true,
            source: UiClipTriggerSource::Lane(LayerId::new("lane")),
            eligible_sources: vec![LayerId::new("lane")],
        };
        let super::TriggerTargetPicker {
            items,
            actions,
            secondary_actions,
            ..
        } = super::trigger_target_picker_items(&LayerId::new("lane"), &[target]);
        assert!(matches!(&secondary_actions[0], Some(PanelAction::Params(
            manifold_ui::ParamsAction::ShowClipTriggerResponse(UiGraphTarget::Generator(id), param)
        )) if id == &LayerId::new("owner") && param.as_ref() == "fire"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "✓ Owner / Fire");
        assert_eq!(items[0].category.as_deref(), Some("Owner"));
        assert!(items[0].thumbnail.is_none());
        assert!(matches!(actions.as_slice(), [
            PanelAction::Params(manifold_ui::ParamsAction::AssignClipTriggerSource(
                UiGraphTarget::Generator(id), param, UiClipTriggerSource::Disabled
            ))
        ] if id == &LayerId::new("owner") && param.as_ref() == "fire"));
    }
}

impl UIRoot {
    /// Called only when a new project snapshot updates the derived catalog.
    /// No version bump: this runs inside the project projection, after the
    /// frame was planned, and the planner already rebuilds an open overlay
    /// with that projection.
    pub(crate) fn refresh_trigger_target_picker(&mut self) {
        let Some(source) = self.browser_popup.persistent_actions_layer().cloned() else {
            return;
        };
        if !self
            .trigger_routing
            .sources
            .iter()
            .any(|item| item.id == source)
        {
            self.browser_popup.close();
        } else {
            let picker = trigger_target_picker_items(&source, &self.trigger_routing.targets);
            self.browser_popup.refresh_actions(
                picker.items,
                picker.categories,
                picker.actions,
                picker.secondary_actions,
            );
        }
    }
}
