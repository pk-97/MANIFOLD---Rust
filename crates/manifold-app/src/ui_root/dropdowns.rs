//! Dropdown / picker builders: the `try_open_dropdown` intercept, the Add-picker
//! item construction, the Ableton picker session, and the audio-channel dropdown
//! builders. Moved verbatim from ui_root/mod.rs (UI_FUNNEL_DECOMPOSITION P-F2a,
//! pure move).

use manifold_ui::{AudioSetupAction, BrowserAction, ClipAction, EditingAction, LayerAction, MappingAction, ParamsAction, ProjectAction, RootAction, TransportAction};
use super::*;

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

/// The `AudioSetSendChannels` action for an explicit channel set (the channel
/// dropdown carries stereo pairing directly now — no separate St/Mo toggle,
/// mono falls out of picking a single channel).
fn send_channels_action(send_id: &manifold_core::AudioSendId, channels: Vec<u16>) -> PanelAction {
    PanelAction::AudioSetup(AudioSetupAction::AudioSetSendChannels(send_id.clone(), channels))
}

/// Push one channel run's rows: a "A + B" stereo-pair item for each adjacent
/// pair, immediately followed by each channel's own single-channel item — so
/// "Left + Right", "Left", "Right" (or "Ch 3+4", "Ch 3", "Ch 4" for unnamed
/// channels) read as one group. An odd channel out at the end of the run gets
/// only its single item (no pair to offer). Shared by the tap and device
/// dropdown builders so the pairing convention can't drift between them.
fn push_channel_pair_rows(
    items: &mut Vec<DropdownItem>,
    send_id: &manifold_core::AudioSendId,
    chans: &[manifold_audio::directory::ChannelInfo],
) {
    let mut i = 0;
    while i < chans.len() {
        if i + 1 < chans.len() {
            let (a, b) = (&chans[i], &chans[i + 1]);
            items.push(
                DropdownItem::new(&format!("{} + {}", a.display_name(), b.display_name()))
                    .with_action(send_channels_action(send_id, vec![a.index, b.index])),
            );
            items.push(
                DropdownItem::new(&a.display_name())
                    .with_action(send_channels_action(send_id, vec![a.index])),
            );
            items.push(
                DropdownItem::new(&b.display_name())
                    .with_action(send_channels_action(send_id, vec![b.index])),
            );
            i += 2;
        } else {
            let a = &chans[i];
            items.push(
                DropdownItem::new(&a.display_name())
                    .with_action(send_channels_action(send_id, vec![a.index])),
            );
            i += 1;
        }
    }
}

/// Channel dropdown for a tap source. Output taps are a fixed stereo mixdown —
/// "Left + Right", "Left", "Right".
fn build_tap_channel_dropdown(send_id: &manifold_core::AudioSendId) -> Vec<DropdownItem> {
    let chans = [
        manifold_audio::directory::ChannelInfo { index: 0, name: Some("Left".into()) },
        manifold_audio::directory::ChannelInfo { index: 1, name: Some("Right".into()) },
    ];
    let mut items = Vec::new();
    push_channel_pair_rows(&mut items, send_id, &chans);
    items
}

/// Build the send-channel dropdown for `device`, grouped by subdevice with
/// platform channel names; each subdevice's channels get stereo-pair rows
/// ("A + B") followed by their single-channel rows, non-selectable headers
/// between groups. Falls back to a single mono entry when no device metadata
/// is available.
fn build_channel_dropdown(
    device: Option<&manifold_audio::directory::DeviceInfo>,
    send_id: &manifold_core::AudioSendId,
) -> Vec<DropdownItem> {
    let fallback = || {
        vec![DropdownItem::new("Channel 1").with_action(send_channels_action(send_id, vec![0]))]
    };
    let Some(device) = device else {
        return fallback();
    };
    if device.channels.is_empty() {
        return fallback();
    }

    let mut items = Vec::new();
    if device.subdevices.is_empty() {
        push_channel_pair_rows(&mut items, send_id, &device.channels);
    } else {
        for group in &device.subdevices {
            items.push(DropdownItem::disabled(&group.name));
            let end = group.channel_start.saturating_add(group.channel_count) as usize;
            let start = group.channel_start as usize;
            if let Some(chans) = device.channels.get(start..end.min(device.channels.len())) {
                push_channel_pair_rows(&mut items, send_id, chans);
            }
        }
    }
    items
}

impl UIRoot {
    /// Open a dropdown whose items carry their own actions (2b.11). No
    /// `DropdownContext` is stored — each item returns
    /// `DropdownAction::SelectedAction`, which the drain fires directly, so there
    /// is no positional index→meaning map to keep in sync.
    pub(crate) fn open_dropdown_typed(&mut self, items: Vec<DropdownItem>, trigger: Rect) {
        self.dropdown_context = None;
        self.dropdown.open(items, trigger, 120.0, &mut self.tree);
        self.overlay_dirty = true;
    }

    /// Refresh the embedded-preset list surfaced into the Add pickers from the
    /// project snapshot. Change-gated by the embedded-preset fingerprint so the
    /// Vec rebuilds only when a fork / import / remove actually changed the set,
    /// not every frame. Called from the per-frame UI sync before event routing.
    pub fn sync_embedded_presets(&mut self, project: &manifold_core::project::Project) {
        let fp = crate::project_io::embedded_presets_fingerprint(project);
        if fp == self.embedded_presets_fingerprint {
            return;
        }
        self.embedded_presets_fingerprint = fp;
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

    /// Classify one `kind`'s Add-picker items by source
    /// (PRESET_LIBRARY_DESIGN P5, D6) — the single place this rule lives, so
    /// `AddEffectClicked` and `GenTypeClicked` can't drift apart:
    /// - **Factory** / **My Library**: every id `preset_type_registry`
    ///   resolves, split by `UserLibrary::is_user_entry` (a file under the
    ///   user root vs. not).
    /// - **This Project**: every `origin: Saved` embedded preset, always
    ///   listed; an `origin: Snapshot` embedded preset ONLY when its id
    ///   resolves nowhere in the registry (its library file is gone) —
    ///   badged "missing from library" rather than "Project" so the browser
    ///   reads as plumbing, not a real project preset.
    ///
    /// `tag_project_category` sets `category: Some("Project")` on the
    /// This-Project items (Effect mode's existing "Project" chip grouping);
    /// Generator mode passes `false`, matching its pre-P5 behavior of never
    /// tagging generator items by category (it renders no category chips).
    fn build_preset_picker_items(
        &self,
        kind: manifold_core::preset_def::PresetKind,
        tag_project_category: bool,
    ) -> Vec<manifold_ui::panels::picker_core::PickerItem> {
        use manifold_core::preset_type_registry;
        use manifold_ui::panels::picker_core::{PickerItem, Source};

        let lib = crate::user_library::UserLibrary::new();
        let available = preset_type_registry::available_of_kind(kind);
        let mut seen_ids: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(available.len());

        let mut items: Vec<PickerItem> = available
            .iter()
            .map(|reg| {
                let is_user = lib.is_user_entry(kind, &reg.id);
                let id = reg.id.as_str().to_string();
                seen_ids.insert(id.clone());
                // PRESET_LIBRARY_DESIGN P6, D7: a My-Library entry's PNG
                // sits beside its JSON (`UserLibrary::thumbnail_path`); a
                // Factory entry's comes from the committed one-shot bin
                // output. `None` (no `Path::is_file` check needed further)
                // when the file simply doesn't exist yet — clean text
                // fallback, never a browse-time render.
                let thumbnail = if is_user {
                    let p = lib.thumbnail_path(kind, reg.id.as_str());
                    p.is_file().then(|| p.to_string_lossy().into_owned())
                } else {
                    manifold_renderer::preset_thumbnail::factory_thumbnail_path(kind, reg.id.as_str())
                        .filter(|p| p.is_file())
                        .map(|p| p.to_string_lossy().into_owned())
                };
                PickerItem {
                    label: reg.display_name.to_string(),
                    type_id: id,
                    category: if tag_project_category {
                        reg.category.map(|c| c.to_string())
                    } else {
                        None
                    },
                    search_text: None,
                    badge: Some(if is_user { "My Library" } else { "Factory" }.to_string()),
                    source: Some(if is_user { Source::MyLibrary } else { Source::Factory }),
                    missing_from_library: false,
                    thumbnail,
                }
            })
            .collect();

        for e in self.embedded_presets.iter().filter(|e| e.kind == kind) {
            use manifold_core::project::EmbeddedOrigin;
            let missing = match e.origin {
                EmbeddedOrigin::Saved => false,
                // A Snapshot whose id already resolves elsewhere (disk file
                // still there) is already represented via `available` above
                // — skip it entirely rather than list it twice.
                EmbeddedOrigin::Snapshot => {
                    if seen_ids.contains(&e.type_id) {
                        continue;
                    }
                    true
                }
            };
            items.push(PickerItem {
                label: e.display_name.clone(),
                type_id: e.type_id.clone(),
                category: if tag_project_category { Some("Project".to_string()) } else { None },
                search_text: None,
                badge: Some(
                    if missing { "missing from library" } else { "Project" }.to_string(),
                ),
                source: Some(Source::Project),
                missing_from_library: missing,
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
    pub(crate) fn try_open_dropdown(&mut self, action: &PanelAction, click_node: Option<NodeId>) -> bool {
        let opened = self.try_open_dropdown_inner(action, click_node);
        if opened {
            self.overlay_dirty = true;
        }
        opened
    }

    fn try_open_dropdown_inner(&mut self, action: &PanelAction, click_node: Option<NodeId>) -> bool {
        let right_click_pos = self.last_right_click_pos;
        let trigger = if let Some(node) = click_node {
            self.tree.get_bounds(node)
        } else {
            Rect::new(100.0, 100.0, 80.0, 24.0)
        };

        match action {
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
                        DropdownItem::new(m.display_name())
                            .with_action(PanelAction::Layer(LayerAction::SetBlendMode(idx.clone(), format!("{:?}", m))))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Clip(ClipAction::ClipDetectQuantizeClicked) => {
                // Typed (2b.11): each grid option carries its quantize step.
                let items: Vec<DropdownItem> =
                    manifold_core::audio_clip_detection::quantize_grid_options()
                        .iter()
                        .map(|(label, step)| {
                            DropdownItem::new(label)
                                .with_action(PanelAction::Clip(ClipAction::ClipDetectSetQuantize(*step)))
                        })
                        .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Clip(ClipAction::ClipDetectLayerClicked(idx)) => {
                // "Auto" (route by trigger name) first, then every candidate
                // layer cached by state_sync — each carries its target layer.
                let mut items = Vec::with_capacity(self.clip_detect_layers.len() + 1);
                items.push(
                    DropdownItem::new("Auto")
                        .with_action(PanelAction::Clip(ClipAction::ClipDetectSetLayer(*idx, None))),
                );
                for (id, name) in &self.clip_detect_layers {
                    items.push(DropdownItem::new(name).with_action(
                        PanelAction::Clip(ClipAction::ClipDetectSetLayer(*idx, Some(id.clone()))),
                    ));
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
                    DropdownItem::new("No source")
                        .with_action(PanelAction::Layer(LayerAction::SetLayerAudioSend(idx.clone(), None))),
                );
                for (id, label) in sends {
                    items.push(
                        DropdownItem::new(&label)
                            .with_action(PanelAction::Layer(LayerAction::SetLayerAudioSend(idx.clone(), Some(id)))),
                    );
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Params(ParamsAction::AddEffectClicked(tab)) => {
                use manifold_core::{preset_def::PresetKind, preset_type_registry};
                use manifold_ui::panels::browser_popup::*;

                // Effect mode keeps its existing "Project" category chip
                // grouping (`tag_project_category: true`).
                let mut items = self.build_preset_picker_items(PresetKind::Effect, true);
                let has_project_items =
                    items.iter().any(|it| it.category.as_deref() == Some("Project"));
                items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));

                // Unique category names (+ "Project" when embedded effects exist).
                let mut cat_names: Vec<String> = preset_type_registry::ALL_CATEGORIES
                    .iter()
                    .map(|&c| c.to_string())
                    .collect();
                if has_project_items {
                    cat_names.push("Project".to_string());
                }

                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Effect,
                    tab: *tab,
                    layer_id: None,
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

                // Generator mode has never rendered category chips (no
                // `category_names` below) — `tag_project_category: false`
                // keeps that unchanged; only the source classification is new.
                let mut items = self.build_preset_picker_items(PresetKind::Generator, false);
                items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));

                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Generator,
                    tab: InspectorTab::Layer,
                    layer_id: layer_id.clone(),
                    items,
                    category_names: Vec::new(),
                    spawn_graph_pos: None,
                    paste_count: 0,
                    screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                });
                true
            }
            PanelAction::Browser(BrowserAction::BrowserCellRightClicked(mode, type_id, source)) => {
                use manifold_ui::panels::picker_core::Source;

                let mut items = Vec::new();
                items.push(
                    DropdownItem::new("Rename…").with_action(PanelAction::Browser(BrowserAction::BrowserRenamePresetClicked(
                        *mode,
                        type_id.clone(),
                        *source,
                    ))),
                );
                if matches!(source, Source::MyLibrary) {
                    items.push(
                        DropdownItem::new("Duplicate").with_action(
                            PanelAction::Browser(BrowserAction::BrowserDuplicatePresetClicked(*mode, type_id.clone())),
                        ),
                    );
                }
                items.push(
                    DropdownItem::new("Delete…").with_action(PanelAction::Browser(BrowserAction::BrowserDeletePresetClicked(
                        *mode,
                        type_id.clone(),
                        *source,
                    ))),
                );
                if matches!(source, Source::MyLibrary) {
                    items.push(
                        DropdownItem::new("Reveal in Finder").with_action(
                            PanelAction::Browser(BrowserAction::BrowserRevealPresetClicked(*mode, type_id.clone())),
                        ),
                    );
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
                    DropdownItem::new("None (video only)")
                        .with_action(PanelAction::Project(ProjectAction::SetAudioInputDevice(String::new()))),
                ];
                items.extend(device_names.into_iter().map(|name| {
                    DropdownItem::new(&name).with_action(PanelAction::Project(ProjectAction::SetAudioInputDevice(name)))
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
                let dir = manifold_audio::directory::system_directory();
                self.audio_setup_devices = dir.list_input_devices();
                let caps = dir.tap_capabilities();
                self.audio_setup_apps =
                    if caps.app_audio { dir.list_audio_apps() } else { Vec::new() };

                // Typed (2b.11): each source row carries its AudioSetDevice action
                // built from the cached metadata; headers stay non-selectable.
                let mut items: Vec<DropdownItem> = Vec::new();

                items.push(
                    DropdownItem::new("System Default")
                        .with_action(PanelAction::AudioSetup(AudioSetupAction::AudioSetDevice(None))),
                );

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
                        let action = PanelAction::AudioSetup(AudioSetupAction::AudioSetDevice(Some(
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
                        let action = PanelAction::AudioSetup(AudioSetupAction::AudioSetDevice(Some(
                            manifold_ui::AudioDeviceRef::app(
                                app.bundle_id.clone(),
                                app.name.clone(),
                            ),
                        )));
                        items.push(DropdownItem::new(&label).with_action(action));
                    }
                }

                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Root(RootAction::AudioSendChannelClicked(send_id)) => {
                // A tap source (system / app output) is a fixed stereo mixdown, so
                // it has no hardware channel layout — present Left/Right. A device
                // source builds its true layout, grouped by subdevice, with
                // platform channel names. Each row carries its typed channel action
                // (2b.11) — the list itself enumerates stereo pairs AND single
                // channels, so mono is just picking one.
                let items = if self
                    .audio_setup_panel
                    .current_device()
                    .is_some_and(|d| d.is_tap())
                {
                    build_tap_channel_dropdown(send_id)
                } else {
                    let dir = manifold_audio::directory::system_directory();
                    let device = match self.audio_setup_panel.current_device() {
                        Some(dev_ref) => dir.resolve(dev_ref.uid_opt(), Some(&dev_ref.name)),
                        // No explicit device → the system default input.
                        None => dir.list_input_devices().into_iter().find(|d| d.is_default),
                    };
                    build_channel_dropdown(device.as_ref(), send_id)
                };
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
                        DropdownItem::new(name)
                            .with_action(PanelAction::Transport(TransportAction::SetMidiClockDevice(i as i32)))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Layer(LayerAction::MidiInputClicked(idx)) => {
                // Typed dropdown (2b.11): item n carries SetMidiNote(idx, n).
                let items: Vec<DropdownItem> = (0..128)
                    .map(|n| {
                        DropdownItem::new(&manifold_core::midi::note_number_to_name(n))
                            .with_action(PanelAction::Project(ProjectAction::SetMidiNote(idx.clone(), n)))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Layer(LayerAction::MidiChannelClicked(idx)) => {
                // "All" (-1) then channels 0..15 (displayed 1..16).
                let mut items: Vec<DropdownItem> = vec![
                    DropdownItem::new("All").with_action(PanelAction::Project(ProjectAction::SetMidiChannel(idx.clone(), -1))),
                ];
                items.extend((1..=16).map(|ch| {
                    DropdownItem::new(&format!("Ch {}", ch))
                        .with_action(PanelAction::Project(ProjectAction::SetMidiChannel(idx.clone(), ch - 1)))
                }));
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Layer(LayerAction::MidiDeviceClicked(idx)) => {
                // "All Devices" (None) then each named device.
                let mut items: Vec<DropdownItem> = vec![
                    DropdownItem::new("All Devices")
                        .with_action(PanelAction::Project(ProjectAction::SetMidiDevice(idx.clone(), None))),
                ];
                items.extend(self.midi_device_names.iter().map(|name| {
                    DropdownItem::new(name)
                        .with_action(PanelAction::Project(ProjectAction::SetMidiDevice(idx.clone(), Some(name.clone()))))
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
                    .enumerate()
                    .map(|(i, r)| {
                        DropdownItem::new(&r.dropdown_label())
                            .with_action(PanelAction::Project(ProjectAction::SetResolution(i)))
                    })
                    .collect();

                // Add display resolutions below presets (Unity: Footer.CollectDisplayResolutions)
                if has_displays {
                    // Separator label (disabled, non-selectable) — matches Unity format
                    items.push(DropdownItem::disabled("---  Displays  ---"));
                    for (w, h, label) in &self.display_resolutions {
                        items.push(
                            DropdownItem::new(&format!("{}  ({}x{})", label, w, h)).with_action(
                                PanelAction::Project(ProjectAction::SetDisplayResolution(*w as i32, *h as i32)),
                            ),
                        );
                    }
                }

                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Params(ParamsAction::MasterExitPathClicked) => {
                // Typed (2b.11): "After All FX" → exit -1, "Before FX" → 0, then each
                // effect → its 1-based exit index.
                let mut items = vec![
                    DropdownItem::new("After All FX")
                        .with_action(PanelAction::Params(ParamsAction::SetLedExitIndex(-1))),
                    DropdownItem::new("Before FX").with_action(PanelAction::Params(ParamsAction::SetLedExitIndex(0))),
                ];
                for (e, name) in self.master_effect_names.iter().enumerate() {
                    items.push(
                        DropdownItem::new(&format!("After {}", name))
                            .with_action(PanelAction::Params(ParamsAction::SetLedExitIndex(e as i32 + 1))),
                    );
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Editing(EditingAction::ClipRightClicked(clip_id)) => {
                // Typed (2b.11): each item carries its clip action.
                let items = vec![
                    DropdownItem::new("Split at Playhead")
                        .with_action(PanelAction::Editing(EditingAction::ContextSplitAtPlayhead(clip_id.clone()))),
                    DropdownItem::new("Delete")
                        .with_action(PanelAction::Editing(EditingAction::ContextDeleteClip(clip_id.clone()))),
                    DropdownItem::new("Duplicate")
                        .with_action(PanelAction::Editing(EditingAction::ContextDuplicateClip(clip_id.clone()))),
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
                let mut items = vec![
                    DropdownItem::new("Paste")
                        .with_action(PanelAction::Editing(EditingAction::ContextPasteAtTrack(*beat, *layer))),
                ];
                if let Some(layer_id) = self
                    .layer_headers
                    .layer_info(*layer)
                    .map(|info| manifold_core::LayerId::new(&info.layer_id))
                {
                    items.push(
                        DropdownItem::new("Import MIDI File")
                            .with_action(PanelAction::Editing(EditingAction::ContextImportMidi(layer_id.clone()))),
                    );
                    items.push(
                        DropdownItem::new("Insert Video Layer")
                            .with_action(PanelAction::Editing(EditingAction::ContextAddVideoLayer(layer_id.clone()))),
                    );
                    items.push(
                        DropdownItem::new("Insert Generator Layer")
                            .with_action(PanelAction::Editing(EditingAction::ContextAddGeneratorLayer(layer_id.clone()))),
                    );
                    items.push(
                        DropdownItem::new("Insert Audio Layer")
                            .with_action(PanelAction::Editing(EditingAction::ContextAddAudioLayer(layer_id))),
                    );
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            // Two-item menu, same typed with_action shape as
            // ClipRightClicked/TrackRightClicked above.
            PanelAction::Editing(EditingAction::AutomationLaneRightClicked(target, param_id)) => {
                let items = vec![
                    DropdownItem::new("Clear Automation").with_action(
                        PanelAction::Project(ProjectAction::ContextClearAutomationLane(target.clone(), param_id.clone())),
                    ),
                    DropdownItem::new("Remove Lane").with_action(
                        PanelAction::Project(ProjectAction::ContextRemoveAutomationLane(target.clone(), param_id.clone())),
                    ),
                ];
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
                let mut items = vec![
                    DropdownItem::new("Paste")
                        .with_action(PanelAction::Editing(EditingAction::ContextPasteAtLayer(layer_id.clone()))),
                ];
                if !is_group {
                    items.push(
                        DropdownItem::new("Import MIDI File")
                            .with_action(PanelAction::Editing(EditingAction::ContextImportMidi(layer_id.clone()))),
                    );
                }
                items.push(
                    DropdownItem::new("Insert Video Layer")
                        .with_action(PanelAction::Editing(EditingAction::ContextAddVideoLayer(layer_id.clone()))),
                );
                items.push(
                    DropdownItem::new("Insert Generator Layer")
                        .with_action(PanelAction::Editing(EditingAction::ContextAddGeneratorLayer(layer_id.clone()))),
                );
                items.push(
                    DropdownItem::new("Insert Audio Layer")
                        .with_action(PanelAction::Editing(EditingAction::ContextAddAudioLayer(layer_id.clone()))),
                );
                items.push(
                    DropdownItem::new("Duplicate Layer")
                        .with_action(PanelAction::Editing(EditingAction::ContextDuplicateLayer(layer_id.clone()))),
                );
                // "Group" only when 2+ non-group, non-nested layers are selected
                let can_group = self.layer_headers.layer_count() >= 2 && !is_group;
                if can_group {
                    items.push(
                        DropdownItem::new("Group Selected Layers")
                            .with_action(PanelAction::Editing(EditingAction::ContextGroupSelectedLayers)),
                    );
                }
                if is_group {
                    items.push(
                        DropdownItem::new("Ungroup")
                            .with_action(PanelAction::Editing(EditingAction::ContextUngroup(layer_id.clone()))),
                    );
                }
                // Only allow delete if more than 1 layer exists
                if self.layer_headers.layer_count() > 1 {
                    items.push(
                        DropdownItem::new("Delete Layer")
                            .with_action(PanelAction::Editing(EditingAction::ContextDeleteLayer(layer_id.clone()))),
                    );
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
                let mut items = Vec::with_capacity(manifold_core::MACRO_COUNT + 3);
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
                    items.push(DropdownItem::new(&label).with_action(
                        PanelAction::Mapping(MappingAction::MapParamToMacro(gpt.clone(), param_id.clone(), i)),
                    ));
                }
                // Ableton picker entry
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                let ableton_connected = self.ableton_session.as_ref().is_some_and(|s| s.connected);
                if ableton_connected {
                    items.push(DropdownItem::new("Map to Ableton Macro…").with_action(
                        PanelAction::Root(RootAction::OpenAbletonPickerForParam(gpt.clone(), param_id.clone())),
                    ));
                } else {
                    items.push(DropdownItem::disabled("Ableton not connected"));
                }
                // "Remove Ableton Mapping" when param is already mapped — the
                // only kind-specific read; the menu + context are unified.
                let is_ableton_mapped = match gpt {
                    GraphParamTarget::Effect(fx_idx) => self.inspector.is_effect_ableton_mapped(
                        self.inspector.last_effect_tab(),
                        *fx_idx,
                        param_id.as_ref(),
                    ),
                    // `is_gen_ableton_mapped` reads the perform inspector's
                    // OWN generator card state (layer-agnostic — no `LayerId`
                    // param) — `GeneratorOf` never actually reaches this arm
                    // in practice (only card-shaped actions like
                    // `ParamLabelRightClick` land here, and the scene panel's
                    // `GeneratorOf` rows never emit them), but the same query
                    // applies if it ever does.
                    GraphParamTarget::Generator | GraphParamTarget::GeneratorOf(_) => {
                        self.inspector.is_gen_ableton_mapped(param_id.as_ref())
                    }
                };
                if is_ableton_mapped {
                    items.push(DropdownItem::new("Remove Ableton Mapping").with_action(
                        PanelAction::Mapping(MappingAction::UnmapParamAbleton(gpt.clone(), param_id.clone())),
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
                    .with_action(PanelAction::Params(ParamsAction::MacroLabelRename(*macro_idx)))
                    .with_separator();
                let mut items = vec![rename];
                if descs.is_empty() {
                    let mut item = DropdownItem::new("No mappings");
                    item.enabled = false;
                    items.push(item);
                } else {
                    for (i, desc) in descs.iter().enumerate() {
                        items.push(
                            DropdownItem::new(desc)
                                .with_action(PanelAction::Mapping(MappingAction::UnmapMacro(*macro_idx, i))),
                        );
                    }
                    if descs.len() > 1 {
                        if let Some(last) = items.last_mut() {
                            last.separator_after = true;
                        }
                        items.push(
                            DropdownItem::new("Clear All")
                                .with_action(PanelAction::Mapping(MappingAction::ClearMacroMappings(*macro_idx))),
                        );
                    }
                }
                // Ableton section — same pattern as effect/gen param dropdowns
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                if self.ableton_session.is_some() {
                    items.push(DropdownItem::new("Map to Ableton Macro\u{2026}").with_action(
                        PanelAction::Mapping(MappingAction::OpenAbletonPickerForMacro(*macro_idx)),
                    ));
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
            PanelAction::Params(ParamsAction::CardRightClicked(gpt)) => {
                // Generators carry Copy/Paste (their own clipboard); both kinds
                // share Make Unique / Export / Import. The menu CONTENTS differ
                // per kind by design (the legitimately-divergent shell); the
                // fork actions + their dispatch are one path keyed by `gpt`.
                // Typed (2b.11): each item carries its action keyed by the card's
                // target, so the dispatch runs one path for effects + generators.
                let mut items = Vec::new();
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
                items.push(
                    DropdownItem::new("Make Unique")
                        .with_action(PanelAction::Params(ParamsAction::MakePresetUnique(gpt.clone()))),
                );
                // Divergence actions (PRESET_LIBRARY_DESIGN D3, P4): only
                // meaningful once the instance has diverged from its library
                // entry (`graph.is_some()`) — reuse the retained card's own
                // `has_graph_mod` bit (the exact source the MOD badge reads),
                // same tab-resolution `is_effect_ableton_mapped` above uses,
                // so there's one source of truth for "is this card diverged"
                // rather than a second computation.
                let has_graph_mod = match gpt {
                    GraphParamTarget::Effect(fx_idx) => {
                        self.inspector.effect_has_graph_mod(self.inspector.last_effect_tab(), *fx_idx)
                    }
                    // Same rationale as `is_ableton_mapped` above: card-only
                    // state, `GeneratorOf` doesn't reach `CardRightClicked` in
                    // practice, same query applies if it ever does.
                    GraphParamTarget::Generator | GraphParamTarget::GeneratorOf(_) => {
                        self.inspector.gen_has_graph_mod()
                    }
                };
                if has_graph_mod {
                    items.push(
                        DropdownItem::new("Revert to Library")
                            .with_action(PanelAction::Params(ParamsAction::RevertToLibrary(gpt.clone()))),
                    );
                    // Wording states the blast radius WITHOUT computing it
                    // (PRESET_LIBRARY_DESIGN §4/§6: counting how many
                    // instances track an id is the forbidden machinery this
                    // design deletes) — "instances", not a computed N.
                    items.push(
                        DropdownItem::new("Push to Library — updates instances tracking this preset")
                            .with_action(PanelAction::Params(ParamsAction::PushToLibrary(gpt.clone()))),
                    );
                }
                // Library doors (PRESET_LIBRARY_DESIGN D4) — explicit "publish a
                // copy" actions, distinct from Make Unique's divergence/retarget.
                items.push(
                    DropdownItem::new("Save to Library…")
                        .with_action(PanelAction::Params(ParamsAction::SaveToLibrary(gpt.clone()))),
                );
                items.push(
                    DropdownItem::new("Save to Project…")
                        .with_action(PanelAction::Params(ParamsAction::SaveToProject(gpt.clone()))),
                );
                items.push(
                    DropdownItem::new("Export Preset…").with_action(PanelAction::Params(ParamsAction::ExportPreset(gpt.clone()))),
                );
                items.push(
                    DropdownItem::new("Import Preset…").with_action(PanelAction::Params(ParamsAction::ImportPreset(gpt.clone()))),
                );
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
                    self.overlay_dirty = true;
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
                    self.overlay_dirty = true;
                    self.ableton_rediscovery_needed = true;
                }
                true
            }
            // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P4, D9: click on a 3+-label
            // enum value cell in the Scene Setup dock opens the shared
            // dropdown — items = the row's label set, checked at the current
            // index, each carrying the SAME `SceneSetupParamChanged` write
            // the dock's steppers already dispatch (no new mutation path).
            // `cell_node_id` resolves the anchor directly (the panel has no
            // `&UITree` in `handle_event` to compute it itself).
            PanelAction::Root(RootAction::SceneSetupEnumClicked {
                layer_id,
                scope_path,
                node_doc_id,
                param_id,
                labels,
                current_index,
                cell_node_id,
            }) => {
                let cell_trigger = self.tree.get_bounds(*cell_node_id);
                let items: Vec<DropdownItem> = labels
                    .iter()
                    .enumerate()
                    .map(|(i, label)| {
                        DropdownItem::new(label)
                            .with_check(i as u32 == *current_index)
                            .with_action(PanelAction::Project(ProjectAction::SceneSetupParamChanged(
                                layer_id.clone(),
                                scope_path.clone(),
                                *node_doc_id,
                                param_id.clone(),
                                i as f32,
                            )))
                    })
                    .collect();
                self.open_dropdown_typed(items, cell_trigger);
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
                let items: Vec<DropdownItem> = labels
                    .iter()
                    .enumerate()
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
            // SCENE_PANEL_UX_DESIGN.md UX-P2, D6: the "+ Add Modifier"
            // button opens the shared dropdown listing the SAME curated
            // vocabulary the old 7-chip grid did — each item dispatches the
            // SAME `SceneSetupAddModifier` action the chips fired directly.
            // `button_node_id` resolves the anchor directly, same
            // resolve-at-open convention as `SceneSetupEnumClicked` above.
            PanelAction::Root(RootAction::SceneSetupAddModifierClicked(layer_id, group_node_id, button_node_id)) => {
                let trigger = self.tree.get_bounds(*button_node_id);
                let items: Vec<DropdownItem> = manifold_ui::panels::scene_setup_panel::MESH_MODIFIER_CHOICES
                    .iter()
                    .map(|(label, type_id)| {
                        DropdownItem::new(label).with_action(PanelAction::Project(ProjectAction::SceneSetupAddModifier(
                            layer_id.clone(),
                            *group_node_id,
                            (*type_id).to_string(),
                        )))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            _ => false,
        }
    }

    /// Convert a color swatch selection into the appropriate PanelAction.
    pub(crate) fn dropdown_color_to_action(
        &self,
        ctx: DropdownContext,
        color_idx: usize,
    ) -> Option<PanelAction> {
        match ctx {
            DropdownContext::LayerContext(layer_id) => {
                let color = manifold_ui::color::COLOR_GRID.get(color_idx)?;
                Some(PanelAction::Editing(EditingAction::ContextSetLayerColor(layer_id, *color)))
            }
        }
    }
}
