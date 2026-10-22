//! The shared text-session seam (BUG-12iyh (UI runner drives editor window,
//! perform mode and text input), text part). `editor_frame.rs`'s shape for
//! an inline text field: how a session begins from a panel action, follows
//! its overlay, takes keys and presses, and commits. The live App
//! (`app_render.rs`, `editor_bridge.rs`, `window_input.rs`, `app.rs`) and the
//! ui-snap script runner call the same functions.
//!
//! What differs is input presence only. The App reads winit keys and measures
//! text once its renderer exists; the runner reads its script and always has
//! a renderer. The graph editor's watched graph and the GPU reach [`commit`]
//! through [`EditorCommitCtx`]; the runner has neither and passes
//! [`EditorCommitCtx::none`], so those commits are no-ops there.

use manifold_core::project::Project;
use manifold_core::Bpm;
use manifold_ui::input::Modifiers;
use manifold_ui::node::Vec2;
use manifold_ui::panels::browser_popup::BrowserPopupAction;
use manifold_ui::panels::{
    AudioSetupAction, ClipAction, LayerAction, MarkerAction, ModulationAction, PanelAction, ParamsAction,
    ProjectAction, RootAction, TransportAction,
};
use manifold_ui::tree::UITree;

use crate::app::SelectionState;
use crate::content_command::ContentCommand;
use crate::text_input::{
    AnchorRect, AudioSendGainParamCtx, DriverFreePeriodCtx, InspectorParamCtx, MaterialColourCtx,
    TextInputField, TextInputState, TextSessionOwner,
};
use crate::ui_root::{OverlayId, UIRoot};

/// One keystroke as a text session reads it. The App maps winit keys into
/// this; the runner maps its `Key` and `Text` steps.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TextKey<'a> {
    Escape,
    Enter,
    Backspace,
    Delete,
    Arrow(Arrow),
    Space,
    Chars(&'a str),
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

impl<'a> TextKey<'a> {
    pub(crate) fn from_winit(key: &'a winit::keyboard::Key) -> Self {
        use winit::keyboard::{Key, NamedKey};
        match key {
            Key::Named(NamedKey::Escape) => Self::Escape,
            Key::Named(NamedKey::Enter) => Self::Enter,
            Key::Named(NamedKey::Backspace) => Self::Backspace,
            Key::Named(NamedKey::Delete) => Self::Delete,
            Key::Named(NamedKey::ArrowLeft) => Self::Arrow(Arrow::Left),
            Key::Named(NamedKey::ArrowRight) => Self::Arrow(Arrow::Right),
            Key::Named(NamedKey::ArrowUp) => Self::Arrow(Arrow::Up),
            Key::Named(NamedKey::ArrowDown) => Self::Arrow(Arrow::Down),
            Key::Named(NamedKey::Space) => Self::Space,
            Key::Character(c) => Self::Chars(c.as_str()),
            _ => Self::Other,
        }
    }
}

/// What a key did to the main window's session.
#[derive(Debug)]
pub(crate) enum KeyOutcome {
    /// No session: the key goes on to the shortcuts.
    NotTyping,
    /// The session took the key.
    Consumed,
    /// The browser search took the key and its results re-filtered.
    Filtered,
    /// The session ended as a commit: run [`commit`] (the App: its handler).
    Committed(TextInputField, String),
}

/// What a left press did to the session.
#[derive(Debug)]
pub(crate) enum PointerDown {
    /// No session.
    Idle,
    /// Inside the field: caret or word placed, drag armed. The press is consumed.
    Inside,
    /// Outside the field: the session ended as a commit, and the press goes on
    /// to its normal target.
    Committed(TextInputField, String),
}

/// The main window's per-frame session upkeep, right after
/// `UIRoot::process_events`: a session dies with the overlay that hosted it
/// (OVERLAY_SESSIONS_AND_PICKER_DESIGN.md D2), the browser popup takes its
/// search session on the frame after it opens, and that session's overlay
/// follows the popup's real search bar (zero until the popup tree exists).
pub(crate) fn pump_main_window(text_input: &mut TextInputState, ui_root: &mut UIRoot) {
    for id in ui_root.take_closed_overlays() {
        text_input.cancel_if_owned_by(TextSessionOwner::MainOverlay(id));
    }
    if ui_root.browser_popup.take_search_focus() {
        text_input.begin_owned(
            TextSessionOwner::MainOverlay(OverlayId::BrowserPopup),
            TextInputField::SearchFilter,
            "",
            AnchorRect::zero(),
            11.0,
        );
    }
    if text_input.owner == Some(TextSessionOwner::MainOverlay(OverlayId::BrowserPopup))
        && text_input.field == TextInputField::SearchFilter
    {
        let r = ui_root.browser_popup.search_bar_rect(&ui_root.tree);
        if r.width > 0.0 && r.height > 0.0 {
            text_input.update_anchor(AnchorRect::new(r.x, r.y, r.width, r.height));
        }
    }
}

fn anchor_of(r: manifold_ui::node::Rect) -> AnchorRect {
    AnchorRect::new(r.x, r.y, r.width, r.height)
}

/// Opens the session a main-window panel action asks for. False when the
/// action opens no text field.
pub(crate) fn begin_for_action(
    text_input: &mut TextInputState,
    ui_root: &mut UIRoot,
    project: &Project,
    selection: &SelectionState,
    action: &PanelAction,
) -> bool {
    match action {
        PanelAction::Root(RootAction::SceneRenameClicked(layer_id, item, name)) => {
            if let Some(r) = ui_root.scene_setup_panel.item_name_rect(&ui_root.tree, item) {
                text_input.scene_object_layer_id = Some(layer_id.clone());
                text_input.scene_item_ref = Some(item.clone());
                text_input.begin(TextInputField::SceneItemRename, name, anchor_of(r), 11.0);
            }
        }
        PanelAction::Params(ParamsAction::BrowserSearchClicked) => {
            let r = ui_root.browser_popup.search_bar_rect(&ui_root.tree);
            text_input.begin_owned(
                TextSessionOwner::MainOverlay(OverlayId::BrowserPopup),
                TextInputField::SearchFilter,
                ui_root.browser_popup.current_filter(),
                anchor_of(r),
                11.0,
            );
        }
        PanelAction::Transport(TransportAction::BpmFieldClicked) => {
            let r = match ui_root.transport.bpm_field_id() {
                Some(id) => ui_root.tree.get_bounds(id),
                None => manifold_ui::node::Rect::new(100.0, 100.0, 120.0, 20.0),
            };
            text_input.begin(TextInputField::Bpm, &format!("{:.1}", project.settings.bpm.0), anchor_of(r), 14.0);
        }
        PanelAction::Transport(TransportAction::FpsFieldClicked) => {
            let r = match ui_root.footer.fps_field_id() {
                Some(id) => ui_root.tree.get_bounds(id),
                None => manifold_ui::node::Rect::new(100.0, 100.0, 120.0, 20.0),
            };
            text_input.begin(TextInputField::Fps, &format!("{:.0}", project.settings.frame_rate), anchor_of(r), 11.0);
        }
        PanelAction::Root(RootAction::BeginColourTextInput { target, param_ids, anchor, value }) => {
            let initial = crate::text_input::format_material_colour_hex(*value);
            text_input.begin(TextInputField::MaterialColour, &initial, anchor_of(*anchor), 11.0);
            text_input.material_colour =
                Some(MaterialColourCtx { target: target.clone(), param_ids: param_ids.clone() });
        }
        PanelAction::Root(RootAction::AudioSendGainBeginTextInput(send_id, value, cell_node_id)) => {
            let r = ui_root.tree.get_bounds(*cell_node_id);
            text_input.begin(TextInputField::AudioSendGainParam, &format!("{value:.1}"), anchor_of(r), 11.0);
            text_input.audio_send_gain_param = Some(AudioSendGainParamCtx { send_id: send_id.clone() });
        }
        PanelAction::Root(RootAction::BeginDriverPeriodTextInput { target, param_id, anchor, value }) => {
            // Whole beats show without a decimal.
            let initial = if value.fract().abs() < 1e-3 {
                format!("{}", value.round() as i64)
            } else {
                format!("{value:.2}")
            };
            text_input.begin(TextInputField::DriverFreePeriod, &initial, anchor_of(*anchor), 11.0);
            text_input.driver_free_period =
                Some(DriverFreePeriodCtx { target: target.clone(), param_id: param_id.clone() });
        }
        PanelAction::Layer(LayerAction::LayerDoubleClicked(id)) => {
            // The commit re-resolves the layer by id (BUG-031); the row index
            // only places this frame's overlay.
            if let Some((pos, layer)) = project.timeline.find_layer_by_id(id) {
                let r = match ui_root.layer_headers.name_node_id(pos) {
                    Some(nid) => ui_root.tree.get_bounds(nid),
                    None => manifold_ui::node::Rect::new(100.0, 100.0, 120.0, 20.0),
                };
                text_input.begin(TextInputField::LayerName, &layer.name, anchor_of(r), 11.0);
                text_input.layer_id = Some(id.clone());
            }
        }
        PanelAction::Marker(MarkerAction::MarkerDoubleClicked(marker_id_str)) => {
            let marker_id = manifold_core::MarkerId::new(marker_id_str.as_str());
            if let Some(marker) = project.timeline.find_marker(&marker_id) {
                // Anchored beside the marker's flag in the ruler.
                let px = ui_root.viewport.beat_to_pixel(marker.beat);
                let ruler = ui_root.viewport.ruler_rect();
                let r = AnchorRect::new(
                    px + manifold_ui::color::MARKER_FLAG_WIDTH * 0.5 + 2.0,
                    ruler.y,
                    80.0,
                    manifold_ui::color::MARKER_FLAG_HEIGHT,
                );
                text_input.begin(TextInputField::MarkerName, &marker.name, r, 9.0);
                text_input.marker_id = Some(marker_id);
            }
        }
        PanelAction::Clip(ClipAction::ClipBpmClicked) => {
            if let Some(clip_id) = selection.primary_selected_clip_id() {
                let bpm_text = project
                    .timeline
                    .layers
                    .iter()
                    .flat_map(|l| l.clips.iter())
                    .find(|c| c.id == *clip_id)
                    .map(|c| {
                        if c.recorded_bpm > 0.0 {
                            format!("{:.1}", c.recorded_bpm)
                        } else {
                            if c.is_audio() { "Unknown" } else { "Auto" }.to_string()
                        }
                    })
                    .unwrap_or_else(|| "Auto".to_string());
                let r = ui_root.inspector.clip_chrome_mut().bpm_button_rect(&ui_root.tree);
                text_input.begin(TextInputField::ClipBpm, &bpm_text, anchor_of(r), 10.0);
            }
        }
        PanelAction::Params(ParamsAction::GenStringParamClicked(sp_idx)) => {
            let entry = ui_root.inspector.gen_params().and_then(|gp| {
                Some((gp.string_param(*sp_idx)?.value.clone(), gp.string_param_rect(&ui_root.tree, *sp_idx)?))
            });
            if let Some((value, r)) = entry {
                text_input.begin(TextInputField::GenStringParam(*sp_idx), &value, anchor_of(r), 11.0);
            }
        }
        PanelAction::Root(RootAction::AudioSendLabelClicked(send_id)) => {
            if let Some(send) = project.audio_setup.find_send(send_id)
                && let Some(r) = ui_root.audio_setup_panel.send_label_rect(&ui_root.tree, send_id)
            {
                text_input.audio_send_id = Some(send_id.clone());
                text_input.begin(TextInputField::AudioSendLabel, &send.label, anchor_of(r), 11.0);
            }
        }
        PanelAction::Params(ParamsAction::MacroLabelRename(idx)) => {
            if let Some(slot) = project.settings.macro_bank.slots.get(*idx)
                && let Some(r) = ui_root.inspector.macro_label_rect(&ui_root.tree, *idx)
            {
                text_input.begin(TextInputField::MacroLabel(*idx), &slot.label, anchor_of(r), 11.0);
            }
        }
        _ => return false,
    }
    true
}

/// Opens a card's numeric or slider-range entry. Both inspector instances emit
/// these; `in_editor` says which window hosts the field, and `tab` is that
/// window's inspector tab. False when the action opens neither.
pub(crate) fn begin_inspector_entry(
    text_input: &mut TextInputState,
    action: &PanelAction,
    in_editor: bool,
    tab: manifold_ui::InspectorTab,
    active_layer: &Option<manifold_core::LayerId>,
    selection: &SelectionState,
    project: &Project,
) -> bool {
    match action {
        PanelAction::Root(RootAction::BeginParamTextInput {
            target, param_id, anchor, value, whole_numbers, degrees, ..
        }) => {
            let display = if *degrees { value.to_degrees() } else { *value };
            let initial = if *whole_numbers {
                format!("{}", display.round() as i64)
            } else {
                format!("{display:.3}")
            };
            text_input.begin(
                if in_editor { TextInputField::EditorInspectorParam } else { TextInputField::InspectorParam },
                &initial,
                anchor_of(*anchor),
                11.0,
            );
            text_input.inspector_param = Some(InspectorParamCtx {
                target: target.clone(),
                param_id: param_id.clone(),
                whole_numbers: *whole_numbers,
                degrees: *degrees,
            });
        }
        PanelAction::Root(RootAction::BeginSliderRangeInput { target, param_id, anchor }) => {
            let param = crate::ui_bridge::resolve_graph_target(target, None, tab, active_layer, selection, project)
                .and_then(|owner| project.graph_target_owner(&owner)?.params.get(param_id.as_ref()).cloned());
            // A param that no longer resolves opens nothing.
            let Some(param) = param else { return true };
            let degrees = param.spec.is_angle;
            let (lo, hi) = param.slider_span();
            let shown = |v: f32| if degrees { v.to_degrees() } else { v };
            text_input.begin(
                if in_editor { TextInputField::EditorSliderRange } else { TextInputField::SliderRange },
                &format!("{} to {}", shown(lo), shown(hi)),
                anchor_of(*anchor),
                11.0,
            );
            text_input.inspector_param = Some(InspectorParamCtx {
                target: target.clone(),
                param_id: param_id.clone(),
                whole_numbers: false,
                degrees,
            });
        }
        _ => return false,
    }
    true
}

/// Runs `f` with a measurer for the session's font. `None` without one: the
/// App measures only once its renderer exists.
fn with_measure<R>(
    text_input: &mut TextInputState,
    tree: Option<&UITree>,
    f: impl FnOnce(&mut TextInputState, &mut dyn FnMut(&str) -> f32) -> R,
) -> Option<R> {
    let tree = tree?;
    let fs = text_input.font_size as u16;
    Some(f(text_input, &mut |s| tree.text_width(s, fs, manifold_ui::FontWeight::Medium)))
}

/// Arrow keys in a session, both windows. Left/Right step a char (Option: a
/// word); Cmd+Left/Right go to the visual line's start/end; Up/Down move a
/// visual line; Cmd+Up/Down go to the text's start/end. Shift extends.
pub(crate) fn arrow(text_input: &mut TextInputState, tree: Option<&UITree>, arrow: Arrow, mods: Modifiers) {
    let (select, word, cmd) = (mods.shift, mods.alt, mods.command);
    match (arrow, cmd) {
        (Arrow::Left, false) => text_input.move_left(select, word),
        (Arrow::Right, false) => text_input.move_right(select, word),
        (Arrow::Up, true) => text_input.move_home(select),
        (Arrow::Down, true) => text_input.move_end(select),
        (Arrow::Left | Arrow::Right, true) => {
            let end = arrow == Arrow::Right;
            with_measure(text_input, tree, |ti, m| ti.move_line_edge(end, select, m));
        }
        (Arrow::Up | Arrow::Down, false) => {
            let down = arrow == Arrow::Down;
            with_measure(text_input, tree, |ti, m| ti.move_vertical(down, select, m));
        }
    }
}

/// A key while a main-window session may be up. Every key goes to the
/// session while it is active; the browser search re-filters on each one.
/// `measurable` is whether text can be measured yet (see [`with_measure`]).
pub(crate) fn main_window_key(
    text_input: &mut TextInputState,
    ui_root: &mut UIRoot,
    key: TextKey<'_>,
    mods: Modifiers,
    measurable: bool,
) -> KeyOutcome {
    if !text_input.active {
        return KeyOutcome::NotTyping;
    }
    let is_search = text_input.field == TextInputField::SearchFilter;
    match key {
        TextKey::Escape => {
            text_input.cancel();
            // One Escape closes the popup too (BUG-022); the closed-overlay
            // pump reconciles the session next frame.
            if is_search {
                ui_root.browser_popup.handle_escape();
            }
        }
        TextKey::Enter => {
            if text_input.multiline && mods.shift {
                text_input.insert_char('\n');
            } else if is_search {
                // Type-and-enter picks straight from the picker; a pick that
                // closes the popup ends the session through the pump.
                if let Some(action) = ui_root.browser_popup.handle_key_nav(manifold_ui::input::Key::Enter) {
                    stash_browser_pick(ui_root, action);
                }
            } else {
                let (field, text) = text_input.commit();
                return KeyOutcome::Committed(field, text);
            }
        }
        TextKey::Backspace => text_input.backspace(),
        TextKey::Delete => text_input.delete(),
        TextKey::Arrow(Arrow::Up) if is_search => {
            ui_root.browser_popup.handle_key_nav(manifold_ui::input::Key::Up);
        }
        TextKey::Arrow(Arrow::Down) if is_search => {
            ui_root.browser_popup.handle_key_nav(manifold_ui::input::Key::Down);
        }
        TextKey::Arrow(dir) => arrow(text_input, measurable.then_some(&ui_root.tree), dir, mods),
        TextKey::Space => text_input.insert_char(' '),
        TextKey::Chars(c) => edit_chars(text_input, c, mods, true),
        TextKey::Other => {}
    }
    if text_input.field == TextInputField::SearchFilter {
        ui_root.set_browser_filter(text_input.text().trim().to_string());
        return KeyOutcome::Filtered;
    }
    KeyOutcome::Consumed
}

/// Typed characters and the Cmd clipboard and select-all shortcuts. `typing`
/// false drops plain characters; the graph editor's own key handler, still on
/// `Application`, drops them under Cmd or Ctrl and is the caller to fold in.
pub(crate) fn edit_chars(text_input: &mut TextInputState, c: &str, mods: Modifiers, typing: bool) {
    if c == "a" && mods.command {
        text_input.select_all_text();
    } else if c == "z" && mods.command {
        text_input.undo_to_seed();
    } else if c == "c" && mods.command {
        let s = text_input.copy_selection();
        if !s.is_empty() {
            crate::macos_pasteboard::set_general_pasteboard_string(&s);
        }
    } else if c == "x" && mods.command {
        let s = text_input.cut_selection();
        if !s.is_empty() {
            crate::macos_pasteboard::set_general_pasteboard_string(&s);
        }
    } else if c == "v" && mods.command {
        if let Some(s) = crate::macos_pasteboard::general_pasteboard_string() {
            text_input.paste(&s);
        }
    } else if typing {
        for ch in c.chars() {
            text_input.insert_char(ch);
        }
    }
}

/// A browser pick from the keyboard becomes the same panel action a click
/// emits, dispatched by `process_events` next frame.
pub(crate) fn stash_browser_pick(ui_root: &mut UIRoot, action: BrowserPopupAction) {
    use manifold_ui::panels::browser_popup::BrowserPopupMode;
    let action = match action {
        BrowserPopupAction::ActionSelected(action) => {
            ui_root.pending_keyboard_actions.push(action);
            return;
        }
        action => action,
    };
    if let BrowserPopupAction::Selected { type_id, mode, tab, layer_id } = action {
        let panel_action = match mode {
            // The layer captured at open rides the pick (PRESET_BROWSER_AUDITION D2).
            BrowserPopupMode::Effect => PanelAction::Params(ParamsAction::AddEffect {
                tab,
                layer_id,
                preset: manifold_ui::types::PresetTypeId::from_string(type_id),
            }),
            BrowserPopupMode::Generator => PanelAction::Project(ProjectAction::SetGenType(
                layer_id,
                manifold_ui::types::PresetTypeId::from_string(type_id),
            )),
            BrowserPopupMode::Node | BrowserPopupMode::Actions => return,
        };
        ui_root.pending_keyboard_actions.push(panel_action);
    }
}

/// A left press while a session may be up (P5b, D16). Inside the field it
/// places the caret (Shift extends; a second press inside the double-click
/// time and radius selects the word) and arms a drag. Outside it commits, and
/// the press still reaches whatever it landed on.
pub(crate) fn pointer_down(
    text_input: &mut TextInputState,
    tree: Option<&UITree>,
    pos: Vec2,
    now: f32,
    shift: bool,
) -> PointerDown {
    if !text_input.active {
        return PointerDown::Idle;
    }
    let inside = with_measure(text_input, tree, |ti, m| ti.overlay_geometry(&mut Vec::new(), m).contains(pos.x, pos.y))
        .unwrap_or(false);
    if !inside {
        let (field, text) = text_input.commit();
        return PointerDown::Committed(field, text);
    }
    let byte = byte_at(text_input, tree, pos);
    let is_double_click = text_input.last_press.is_some_and(|(t, px, py)| {
        (now - t) < manifold_ui::color::DOUBLE_CLICK_TIME_SEC
            && (pos.x - px).powi(2) + (pos.y - py).powi(2) < manifold_ui::color::DOUBLE_CLICK_RADIUS_PX.powi(2)
    });
    text_input.last_press = Some((now, pos.x, pos.y));
    if is_double_click {
        text_input.select_word_at(byte);
    } else {
        text_input.caret_to(byte, shift);
    }
    text_input.dragging = true;
    PointerDown::Inside
}

/// Pointer motion while a session's drag is armed extends the selection.
/// False (not consumed) when no drag is armed.
pub(crate) fn pointer_move(text_input: &mut TextInputState, tree: Option<&UITree>, pos: Vec2) -> bool {
    if !text_input.active || !text_input.dragging {
        return false;
    }
    let byte = byte_at(text_input, tree, pos);
    text_input.drag_to(byte);
    true
}

/// A release ends a session's drag.
pub(crate) fn pointer_up(text_input: &mut TextInputState) {
    text_input.dragging = false;
}

fn byte_at(text_input: &mut TextInputState, tree: Option<&UITree>, pos: Vec2) -> usize {
    let len = text_input.text().len();
    with_measure(text_input, tree, |ti, m| ti.byte_at_point(pos.x, pos.y, m)).unwrap_or(len)
}

/// What a graph-editor or preset-save commit reads beyond the dispatch
/// context: the editor's watched graph (target, catalog default, open group
/// scope) and the UI-side GPU for the save-time thumbnail.
pub(crate) struct EditorCommitCtx<'a> {
    pub target: Option<&'a manifold_core::GraphTarget>,
    pub catalog_default: Option<&'a manifold_core::effect_graph_def::EffectGraphDef>,
    pub scope: &'a [u32],
    pub gpu: Option<&'a manifold_node_engine::gpu::context::GpuContext>,
}

impl EditorCommitCtx<'_> {
    #[cfg(any(test, feature = "ui-snapshot"))]
    pub(crate) const fn none() -> Self {
        EditorCommitCtx { target: None, catalog_default: None, scope: &[], gpu: None }
    }

    /// The watched target and its catalog default, both or neither.
    fn graph(&self) -> Option<(manifold_core::GraphTarget, manifold_core::effect_graph_def::EffectGraphDef)> {
        Some((self.target?.clone(), self.catalog_default?.clone()))
    }
}

/// Commits `text` into `field` through the dispatch context. The graph
/// canvas renders from the content thread's snapshot, so graph fields send to
/// the content thread only.
pub(crate) fn commit<'a>(
    text_input: &mut TextInputState,
    field: TextInputField,
    text: &str,
    cx: &mut crate::ui_bridge::DispatchCtx<'a>,
    ed: &EditorCommitCtx<'a>,
) {
    match field {
        TextInputField::Bpm => {
            if let Ok(new_bpm) = text.parse::<f32>() {
                let new_bpm = Bpm(new_bpm.clamp(20.0, 300.0));
                if (cx.project.settings.bpm.0 - new_bpm.0).abs() >= 0.01 {
                    let cmd = manifold_editing::commands::settings::ChangeBpmCommand::master(&cx.project, new_bpm);
                    ContentCommand::send(cx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
                }
            }
        }
        TextInputField::Fps => {
            if let Ok(fps) = text.parse::<f32>() {
                let fps = fps.clamp(1.0, 240.0);
                cx.execute(Box::new(manifold_editing::commands::settings::ChangeFrameRateCommand::new(
                    cx.project.settings.frame_rate,
                    fps,
                )));
                // The content thread renders at project FPS; the UI runs at display rate.
                ContentCommand::send(cx.content_tx, ContentCommand::SetFrameRate(fps as f64));
            }
        }
        TextInputField::SliderRange | TextInputField::EditorSliderRange => {
            if let Some(ctx) = text_input.inspector_param.take()
                && let Some(range) = crate::text_input::parse_slider_range(text)
            {
                let range = range.map(|(lo, hi)| {
                    let stored = |v| crate::text_input::degrees_commit_value(v, ctx.degrees);
                    (stored(lo), stored(hi))
                });
                let action = PanelAction::Params(ParamsAction::SetSliderRange(ctx.target, ctx.param_id, range));
                let _ = crate::ui_bridge::dispatch(&action, cx);
            }
        }
        TextInputField::InspectorParam | TextInputField::EditorInspectorParam => {
            if let Some(ctx) = text_input.inspector_param.take()
                && let Some(parsed) = crate::text_input::parse_lenient_numeric(text)
            {
                // A typed value may exceed the slider's travel
                // (PARAM_RANGE_CONTRACT_DESIGN.md D3). Angle rows type degrees.
                let mut v = crate::text_input::degrees_commit_value(parsed, ctx.degrees);
                if ctx.whole_numbers {
                    v = v.round();
                }
                // The slider drag's own scrub wire, so the entry is one undo step.
                use manifold_ui::panels::{ScrubPhase, ScrubValue, ValueRef};
                for act in [
                    PanelAction::Scrub(ValueRef::Param(ctx.target.clone(), ctx.param_id.clone()), ScrubPhase::Begin),
                    PanelAction::Scrub(
                        ValueRef::Param(ctx.target.clone(), ctx.param_id.clone()),
                        ScrubPhase::Move(ScrubValue::Scalar(v)),
                    ),
                    PanelAction::Scrub(ValueRef::Param(ctx.target, ctx.param_id), ScrubPhase::Commit),
                ] {
                    let _ = crate::ui_bridge::dispatch(&act, cx);
                }
            }
        }
        TextInputField::MaterialColour => {
            if let Some(ctx) = text_input.material_colour.take()
                && let Some(values) = crate::text_input::parse_material_colour_hex(text)
            {
                // The swatch drag's phased RGB scrub: one undo command.
                for action in crate::text_input::material_colour_scrub_actions(ctx, values) {
                    let _ = crate::ui_bridge::dispatch(&action, cx);
                }
            }
        }
        TextInputField::AudioSendGainParam => {
            if let Some(ctx) = text_input.audio_send_gain_param.take()
                && let Some(parsed) = crate::text_input::parse_lenient_numeric(text)
            {
                let act = PanelAction::AudioSetup(AudioSetupAction::AudioSendGainSetTyped(ctx.send_id, parsed));
                let _ = crate::ui_bridge::dispatch(&act, cx);
            }
        }
        TextInputField::DriverFreePeriod => {
            // Only a positive period means anything; anything else leaves the
            // driver in its prior mode.
            if let Some(ctx) = text_input.driver_free_period.take()
                && let Some(parsed) = crate::text_input::parse_lenient_numeric(text)
                && parsed > 0.0
            {
                use manifold_ui::panels::DriverConfigAction;
                let act = PanelAction::Modulation(ModulationAction::DriverConfig(
                    ctx.target,
                    ctx.param_id,
                    DriverConfigAction::SetFreePeriod(parsed),
                ));
                let _ = crate::ui_bridge::dispatch(&act, cx);
            }
        }
        TextInputField::LayerName => {
            // By id, so a layer-list change since the double-click cannot
            // rename the wrong row (BUG-031).
            if let Some(layer_id) = text_input.layer_id.take()
                && let Some((_, layer)) = cx.project.timeline.find_layer_by_id(layer_id.as_str())
                && layer.name != text
            {
                let old_name = layer.name.clone();
                cx.execute(Box::new(manifold_editing::commands::layer::RenameLayerCommand::new(
                    layer_id,
                    old_name,
                    text.to_string(),
                )));
            }
        }
        TextInputField::ClipBpm => {
            // Empty or "auto" is 0: follow the project BPM. Unparseable text
            // changes nothing.
            let trimmed = text.trim();
            let new_bpm = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
                0.0
            } else if let Ok(v) = trimmed.parse::<f32>() {
                if v > 0.0 { v.clamp(20.0, 300.0) } else { 0.0 }
            } else {
                return;
            };
            if let Some(clip_id) = cx.selection.primary_selected_clip_id() {
                let clip_id = clip_id.clone();
                let clip = cx.project.find_clip_by_id(&clip_id);
                let old_bpm = clip.map(|c| c.recorded_bpm).unwrap_or(0.0);
                let enabling = new_bpm > 0.0
                    && clip.is_some_and(|clip| {
                        clip.is_audio() && (!clip.is_audio_warp_enabled() || clip.audio_bpm_automatic)
                    });
                if (old_bpm - new_bpm).abs() >= 0.01 || enabling {
                    let cmd = manifold_editing::commands::clip::ChangeClipRecordedBpmCommand::new(clip_id, old_bpm, new_bpm);
                    ContentCommand::send(cx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
                }
            }
        }
        TextInputField::MacroLabel(idx) => {
            let new_label = text.trim().to_string();
            if let Some(slot) = cx.project.settings.macro_bank.slots.get(idx)
                && slot.label != new_label
            {
                let old_label = slot.label.clone();
                cx.execute(Box::new(manifold_editing::commands::settings::RenameMacroLabelCommand::new(
                    idx, old_label, new_label,
                )));
            }
        }
        TextInputField::GenStringParam(sp_idx) => {
            // The key comes from the generator's definition; the old value
            // from the selected clip on the active layer, else its first clip.
            if let Some(layer) = cx
                .active_layer
                .as_ref()
                .and_then(|id| cx.project.timeline.find_layer_by_id(id))
                .map(|(_, l)| l)
                && let Some(def) = manifold_core::preset_definition_registry::try_get(layer.generator_type())
                && let Some(sp_def) = def.string_param_defs.get(sp_idx)
            {
                let key = sp_def.key.to_string();
                let new_value = (!text.is_empty()).then(|| text.to_string());
                let clip = cx
                    .selection
                    .primary_selected_clip_id()
                    .and_then(|sel_id| layer.clips.iter().find(|c| c.id == *sel_id))
                    .or_else(|| layer.clips.first());
                let (clip_id, old_value) = clip
                    .map(|c| (c.id.clone(), c.string_params.as_ref().and_then(|m| m.get(&key)).cloned()))
                    .unwrap_or_default();
                if old_value != new_value {
                    cx.execute(Box::new(manifold_editing::commands::clip::SetClipStringParamCommand::new(
                        clip_id, key, old_value, new_value,
                    )));
                }
            }
        }
        TextInputField::SearchFilter => {
            // The browser filter only; no undo command.
            cx.ui.set_browser_filter(text.trim().to_string());
        }
        TextInputField::MarkerName => {
            if let Some(marker_id) = text_input.marker_id.take() {
                let old_name = cx.project.timeline.find_marker(&marker_id).map(|m| m.name.clone()).unwrap_or_default();
                if old_name != text {
                    cx.execute(Box::new(manifold_editing::commands::marker::RenameMarkerCommand::new(
                        marker_id,
                        old_name,
                        text.to_string(),
                    )));
                }
            }
        }
        TextInputField::AudioSendLabel => {
            if let Some(send_id) = text_input.audio_send_id.take() {
                let new_label = text.trim().to_string();
                let old_label =
                    cx.project.audio_setup.find_send(&send_id).map(|s| s.label.clone()).unwrap_or_default();
                if !new_label.is_empty() && old_label != new_label {
                    cx.execute(Box::new(manifold_editing::commands::audio_setup::RenameAudioSendCommand::new(
                        send_id, old_label, new_label,
                    )));
                }
            }
        }
        TextInputField::SceneItemRename => {
            if let Some(layer_id) = text_input.scene_object_layer_id.take()
                && let Some(item) = text_input.scene_item_ref.take()
            {
                match crate::scene_edit::rename(&cx.project, &layer_id, &item, text) {
                    Ok(Some(edit)) => ContentCommand::send(cx.content_tx, ContentCommand::ExecuteOnContent(edit.command)),
                    Ok(None) => {}
                    Err(reason) => ContentCommand::send(cx.content_tx, ContentCommand::GraphEditRejected(reason)),
                }
            }
        }
        TextInputField::RenamePreset => {
            // My Library renames the file (no undo, as Push to Library); a
            // project preset renames through an undoable command
            // (PRESET_LIBRARY_DESIGN D6).
            if let Some(ctx) = text_input.rename_preset.take() {
                let typed = text.trim();
                if typed.is_empty() {
                    log::warn!("[preset] Rename cancelled: empty name");
                } else {
                    use manifold_ui::panels::picker_core::Source;
                    match ctx.source {
                        Source::MyLibrary => {
                            if let Err(e) = crate::user_library::UserLibrary::new().rename(ctx.kind, &ctx.id, typed) {
                                log::error!("[preset] rename failed: {e}");
                            }
                        }
                        Source::Project => {
                            cx.execute(Box::new(manifold_editing::commands::preset::RenameEmbeddedPresetCommand::new(
                                ctx.id,
                                typed.to_string(),
                            )));
                        }
                        Source::Factory => log::error!("[preset] rename requested for a Factory id — unreachable"),
                    }
                }
            }
        }
        TextInputField::AutomationPointValue | TextInputField::AutomationPointTime => {
            crate::app::automation_point_input::commit(text_input, cx, field, text);
        }
        TextInputField::GraphGroupRename(group_node_id) => {
            let new_handle = text.trim().to_string();
            if !new_handle.is_empty()
                && let Some((target, default)) = ed.graph()
            {
                let cmd = manifold_editing::commands::graph::RenameGroupCommand::new(
                    target,
                    ed.scope.to_vec(),
                    group_node_id,
                    new_handle,
                    default,
                );
                ContentCommand::send(cx.content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
        }
        TextInputField::GraphStringParam(node_id) => {
            if let Some(param_name) = text_input.graph_param_name.take()
                && let Some((target, default)) = ed.graph()
            {
                let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                    target,
                    node_id,
                    param_name,
                    manifold_core::effect_graph_def::SerializedParamValue::String { value: text.to_string() },
                    default,
                )
                .with_scope(ed.scope.to_vec());
                ContentCommand::send(cx.content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
        }
        TextInputField::GraphWgsl(node_id) => {
            if let Some((target, default)) = ed.graph() {
                let cmd = manifold_editing::commands::graph::SetWgslSourceCommand::new(
                    target,
                    node_id,
                    text.to_string(),
                    default,
                )
                .with_scope(ed.scope.to_vec());
                ContentCommand::send(cx.content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
        }
        TextInputField::GraphNumericParam(node_id) => {
            if let Some(ctx) = text_input.graph_numeric_param.take() {
                // Lenient parse, same convention as InspectorParam: keep only
                // the numeric head so a value typed with a unit suffix commits.
                let cleaned: String = text
                    .trim()
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
                    .collect();
                if let Ok(parsed) = cleaned.parse::<f32>() {
                    let mut v = parsed.clamp(ctx.min, ctx.max);
                    if ctx.whole_numbers {
                        v = v.round();
                    }
                    if let Some(outer_param_id) = ctx.outer_param_id {
                        // A group-face mirror row writes through the outer card
                        // param's own path, the same dispatch
                        // `GraphEditCommand::SetOuterParam` uses.
                        if let Some(target) = ed.target {
                            use manifold_ui::panels::{GraphParamTarget, ScrubPhase, ScrubValue, ValueRef};
                            let gpt = match target {
                                manifold_core::GraphTarget::Effect(eid) => GraphParamTarget::Effect(eid.clone()),
                                manifold_core::GraphTarget::Generator(_)
                                | manifold_core::GraphTarget::SceneModifier { .. } => GraphParamTarget::Generator,
                            };
                            let action = PanelAction::Scrub(
                                ValueRef::Param(gpt, manifold_core::effects::ParamId::from(outer_param_id)),
                                ScrubPhase::Move(ScrubValue::Scalar(v)),
                            );
                            let prev = cx.editor_target.replace(target);
                            let _ = crate::ui_bridge::dispatch(&action, cx);
                            cx.editor_target = prev;
                        }
                    } else if let Some((target, default)) = ed.graph() {
                        let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                            target,
                            node_id,
                            ctx.param_name,
                            manifold_core::effect_graph_def::SerializedParamValue::Float { value: v },
                            default,
                        )
                        .with_scope(ed.scope.to_vec());
                        ContentCommand::send(cx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
        }
        // Live-filtered while typing (see the editor key handler); the final
        // query persists as the canvas highlight. Nothing to commit.
        TextInputField::GraphNodeSearch => {}
        TextInputField::GraphTableCell => {
            if let Some(edit) = text_input.graph_table_edit.take()
                && let Ok(v) = text.trim().parse::<f32>()
                && let Some((target, default)) = ed.graph()
            {
                let mut rows = edit.rows;
                if let Some(cell) = rows.get_mut(edit.row).and_then(|r| r.get_mut(edit.col)) {
                    // A no-op edit skips the command so a stray click + Enter
                    // doesn't push an empty undo step.
                    if (*cell - v).abs() > f32::EPSILON {
                        *cell = v;
                        let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                            target,
                            edit.node_id,
                            edit.param_name,
                            manifold_core::effect_graph_def::SerializedParamValue::Table { rows },
                            default,
                        )
                        .with_scope(ed.scope.to_vec());
                        ContentCommand::send(cx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
        }
        TextInputField::SavePresetName => {
            // Save to Library / Save to Project. The ctx carries the target's
            // effective definition, resolved when the prompt opened; this only
            // mints the name and writes or upserts it.
            if let Some(ctx) = text_input.save_preset.take() {
                let typed = text.trim();
                if typed.is_empty() {
                    log::warn!("[preset] Save to {:?} cancelled: empty name", ctx.destination);
                } else {
                    match ctx.destination {
                        crate::text_input::SavePresetDestination::Library => {
                            let lib = crate::user_library::UserLibrary::new();
                            match lib.save(ctx.kind, typed, &ctx.def) {
                                Ok(id) => {
                                    log::info!("[preset] saved '{}' to the user library", id.as_str());
                                    // The only thumbnail render: save-time, once. The
                                    // browser reads this PNG back off disk.
                                    if let Some(gpu) = ed.gpu {
                                        let png_path = lib.thumbnail_path(ctx.kind, id.as_str());
                                        if let Err(e) = manifold_compositor::preset_thumbnail::render_preset_thumbnail_to_file(
                                            &gpu.device,
                                            ctx.kind,
                                            &ctx.def,
                                            manifold_compositor::preset_thumbnail::THUMBNAIL_WIDTH,
                                            manifold_compositor::preset_thumbnail::THUMBNAIL_HEIGHT,
                                            &png_path,
                                        ) {
                                            log::error!("[preset] thumbnail render failed for '{}': {e}", id.as_str());
                                        }
                                    }
                                }
                                Err(e) => log::error!("[preset] Save to Library failed: {e}"),
                            }
                        }
                        crate::text_input::SavePresetDestination::Project => {
                            let mut def = ctx.def;
                            if let Some(meta) = def.preset_metadata.as_mut() {
                                meta.display_name = typed.to_string();
                            }
                            cx.execute(Box::new(manifold_editing::commands::preset::SaveToProjectCommand::new(ctx.kind, def)));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The BPM field end to end through the seam: the click opens it with the
    /// value selected, typing replaces it, Enter commits, and the commit sends
    /// one content command.
    #[test]
    fn bpm_field_opens_types_and_commits() {
        let mut ui = UIRoot::new();
        let mut project = Project::default();
        let mut selection = SelectionState::default();
        let mut text_input = TextInputState::new();
        let action = PanelAction::Transport(TransportAction::BpmFieldClicked);
        assert!(begin_for_action(&mut text_input, &mut ui, &project, &selection, &action));
        assert_eq!(text_input.field, TextInputField::Bpm);
        for c in ["1", "3", "0"] {
            assert!(matches!(main_window_key(&mut text_input, &mut ui, TextKey::Chars(c), Modifiers::NONE, false), KeyOutcome::Consumed));
        }
        let KeyOutcome::Committed(field, text) = main_window_key(&mut text_input, &mut ui, TextKey::Enter, Modifiers::NONE, false)
        else {
            panic!("Enter commits a plain field");
        };
        assert_eq!((field, text.as_str()), (TextInputField::Bpm, "130"));
        assert!(!text_input.active);

        let (tx, rx) = crossbeam_channel::unbounded();
        let mut edits = crate::gesture_preview::UiEdits::default();
        let content_state = crate::content_state::ContentState::default();
        let mut active_layer = None;
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let mut cx = crate::ui_bridge::DispatchCtx {
            project: crate::ui_bridge::ProjectRef::new(&mut project, &mut edits, &tx),
            content_tx: &tx,
            content_state: &content_state,
            ui: &mut ui,
            selection: &mut selection,
            active_layer: &mut active_layer,
            user_prefs: &mut user_prefs,
            editor_target: None,
            scrub: &mut scrub,
            dialogs: Default::default(),
        };
        commit(&mut text_input, field, &text, &mut cx, &EditorCommitCtx::none());
        assert!(matches!(rx.try_recv(), Ok(ContentCommand::ExecuteOnContent(_))));
    }

    /// Escape cancels without a commit; with no session keys pass through.
    #[test]
    fn escape_cancels_and_idle_keys_pass_through() {
        let mut ui = UIRoot::new();
        let mut text_input = TextInputState::new();
        assert!(matches!(main_window_key(&mut text_input, &mut ui, TextKey::Escape, Modifiers::NONE, false), KeyOutcome::NotTyping));
        text_input.begin(TextInputField::MarkerName, "Drop", AnchorRect::zero(), 9.0);
        assert!(matches!(main_window_key(&mut text_input, &mut ui, TextKey::Escape, Modifiers::NONE, false), KeyOutcome::Consumed));
        assert!(!text_input.active);
    }

    /// A press with nothing to measure lands outside the field: the session
    /// commits and the press goes on.
    #[test]
    fn press_outside_commits() {
        let mut text_input = TextInputState::new();
        assert!(matches!(pointer_down(&mut text_input, None, Vec2::ZERO, 0.0, false), PointerDown::Idle));
        text_input.begin(TextInputField::LayerName, "Bass", AnchorRect::zero(), 11.0);
        let PointerDown::Committed(field, text) = pointer_down(&mut text_input, None, Vec2::ZERO, 0.0, false) else {
            panic!("a press outside commits");
        };
        assert_eq!((field, text.as_str()), (TextInputField::LayerName, "Bass"));
    }

    /// One commit through the seam with the given editor context: the content
    /// commands it sent, and the project afterwards.
    fn commit_collect(
        text_input: &mut TextInputState,
        project: &mut Project,
        field: TextInputField,
        text: &str,
        ed: &EditorCommitCtx<'_>,
    ) -> (Vec<ContentCommand>, UIRoot, SelectionState) {
        let mut ui = UIRoot::new();
        let mut selection = SelectionState::default();
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut edits = crate::gesture_preview::UiEdits::default();
        let content_state = crate::content_state::ContentState::default();
        let mut active_layer = None;
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let mut cx = crate::ui_bridge::DispatchCtx {
            project: crate::ui_bridge::ProjectRef::new(project, &mut edits, &tx),
            content_tx: &tx,
            content_state: &content_state,
            ui: &mut ui,
            selection: &mut selection,
            active_layer: &mut active_layer,
            user_prefs: &mut user_prefs,
            editor_target: None,
            scrub: &mut scrub,
            dialogs: Default::default(),
        };
        commit(text_input, field, text, &mut cx, ed);
        (rx.try_iter().collect(), ui, selection)
    }

    fn empty_def() -> manifold_core::effect_graph_def::EffectGraphDef {
        serde_json::from_str(r#"{"version":1,"nodes":[],"wires":[]}"#).expect("minimal graph def")
    }

    fn watched_ctx<'a>(
        target: &'a manifold_core::GraphTarget,
        default: &'a manifold_core::effect_graph_def::EffectGraphDef,
    ) -> EditorCommitCtx<'a> {
        EditorCommitCtx { target: Some(target), catalog_default: Some(default), scope: &[], gpu: None }
    }

    fn executes(cmds: &[ContentCommand]) -> usize {
        cmds.iter().filter(|c| matches!(c, ContentCommand::Execute(_))).count()
    }

    /// Each graph-editor field commits exactly one `Execute` to the content
    /// thread through the seam, and nothing without a watched graph.
    #[test]
    fn graph_fields_execute_on_the_content_thread() {
        use crate::text_input::{GraphNumericParamCtx, TableCellEdit};
        let target = manifold_core::GraphTarget::Generator(manifold_core::LayerId::new("g"));
        let default = empty_def();
        let ed = watched_ctx(&target, &default);
        let mut project = Project::default();

        let mut ti = TextInputState::new();
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::GraphGroupRename(3), " Core ", &ed);
        assert_eq!(executes(&cmds), 1, "group rename");
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::GraphGroupRename(3), "   ", &ed);
        assert!(cmds.is_empty(), "an empty rename sends nothing");

        ti.graph_param_name = Some("label".into());
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::GraphStringParam(3), "hi", &ed);
        assert_eq!(executes(&cmds), 1, "string param");
        assert!(ti.graph_param_name.is_none(), "the param name is taken");

        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::GraphWgsl(3), "fn main() {}", &ed);
        assert_eq!(executes(&cmds), 1, "wgsl source");

        ti.graph_numeric_param = Some(GraphNumericParamCtx {
            param_name: "gain".into(),
            min: 0.0,
            max: 1.0,
            whole_numbers: false,
            outer_param_id: None,
        });
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::GraphNumericParam(3), "0.5x", &ed);
        assert_eq!(executes(&cmds), 1, "numeric param keeps its numeric head");

        ti.graph_table_edit = Some(TableCellEdit {
            node_id: 3,
            param_name: "table".into(),
            row: 0,
            col: 1,
            rows: vec![vec![0.0, 1.0]],
        });
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::GraphTableCell, "1.0", &ed);
        assert!(cmds.is_empty(), "an unchanged cell pushes no undo step");
        ti.graph_table_edit = Some(TableCellEdit {
            node_id: 3,
            param_name: "table".into(),
            row: 0,
            col: 1,
            rows: vec![vec![0.0, 1.0]],
        });
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::GraphTableCell, "2.0", &ed);
        assert_eq!(executes(&cmds), 1, "table cell");

        let none = EditorCommitCtx::none();
        ti.graph_param_name = Some("label".into());
        for field in [TextInputField::GraphGroupRename(3), TextInputField::GraphStringParam(3), TextInputField::GraphWgsl(3)] {
            let (cmds, ..) = commit_collect(&mut ti, &mut project, field, "x", &none);
            assert!(cmds.is_empty(), "{field:?} without a watched graph");
        }
    }

    /// The node search keeps its query as the canvas highlight: nothing to
    /// commit, and it reaches the seam.
    #[test]
    fn graph_node_search_commits_nothing() {
        let mut project = Project::default();
        let mut ti = TextInputState::new();
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::GraphNodeSearch, "blur", &EditorCommitCtx::none());
        assert!(cmds.is_empty());
    }

    /// Save to Project mints a project preset through one undoable command;
    /// an empty name saves nothing.
    #[test]
    fn save_preset_name_to_project_executes_one_command() {
        use crate::text_input::{SavePresetCtx, SavePresetDestination};
        let mut project = Project::default();
        let mut ti = TextInputState::new();
        let ctx = || SavePresetCtx {
            kind: manifold_core::preset_def::PresetKind::Effect,
            def: empty_def(),
            destination: SavePresetDestination::Project,
        };
        ti.save_preset = Some(ctx());
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::SavePresetName, "  ", &EditorCommitCtx::none());
        assert!(cmds.is_empty() && project.embedded_presets.is_empty(), "empty name cancels");
        ti.save_preset = Some(ctx());
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::SavePresetName, "My Look", &EditorCommitCtx::none());
        assert_eq!(executes(&cmds), 1);
        assert_eq!(project.embedded_presets.len(), 1, "the local project is written too");
        assert!(ti.save_preset.is_none());
    }

    /// An automation point's value and time commit through `MoveAutomationPoint`
    /// and re-select the moved point; bad text reopens the field and changes
    /// nothing.
    #[test]
    fn automation_point_fields_commit_through_the_seam() {
        use crate::text_input::AutomationPointEditCtx;
        use manifold_core::effects::{AutomationLane, AutomationPoint, PresetInstance, SegmentShape};
        use manifold_core::Beats;

        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Mirror"));
        fx.init_defaults();
        let target = manifold_ui::view::UiGraphTarget::Effect(fx.id.clone());
        let (min, max) = fx.params.get("amount").unwrap().slider_span();
        let at = |beat: f64, value: f32| AutomationPoint { beat: Beats(beat), value, shape: SegmentShape::Linear };
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: vec![at(0.0, min), at(4.0, min + (max - min) * 0.5)],
        }]);
        let mut project = Project::default();
        project.settings.master_effects.push(fx);
        let value_norm = 0.5;
        let open = |ti: &mut TextInputState, field: TextInputField| {
            ti.begin(field, "x", AnchorRect::zero(), 11.0);
            ti.automation_point_edit = Some(AutomationPointEditCtx {
                target: target.clone(),
                param_id: "amount".into(),
                original_beat: Beats(4.0),
                original_value_norm: value_norm,
                param_min: min,
                param_max: max,
                whole_numbers: false,
                beats_per_bar: 4,
            });
        };
        let lane_points = |p: &Project| p.settings.master_effects[0].automation_lanes.as_ref().unwrap()[0].points.clone();
        let none = EditorCommitCtx::none();

        let mut ti = TextInputState::new();
        open(&mut ti, TextInputField::AutomationPointValue);
        let (cmds, ..) = commit_collect(&mut ti, &mut project, TextInputField::AutomationPointValue, "nope", &none);
        assert!(cmds.is_empty() && ti.active && ti.automation_point_edit.is_some(), "bad value reopens the field");
        assert_eq!(lane_points(&project)[1].beat, Beats(4.0));

        ti.cancel();
        open(&mut ti, TextInputField::AutomationPointTime);
        let (cmds, _, selection) = commit_collect(&mut ti, &mut project, TextInputField::AutomationPointTime, "3.1.000", &none);
        assert_eq!(executes(&cmds), 1, "time commit");
        assert_eq!(lane_points(&project)[1].beat, Beats(8.0), "bar 3 beat 1 at 4/4");
        assert_eq!(selection.selected_automation_point.as_ref().map(|p| p.beat), Some(Beats(8.0)));
        assert!(ti.automation_point_edit.is_none());
    }
}
