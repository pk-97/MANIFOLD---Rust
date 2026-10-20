//! The shared graph-editor input seam (BUG-12iyh (UI runner drives editor
//! window, perform mode and text input), editor input part). `editor_frame.rs`
//! builds and draws the editor window; this module takes its input: pointer
//! moves, buttons, the wheel and pinch into the card lane, the canvas, the
//! dock, the mini-timeline and both mapping popovers, then the per-frame
//! drain that turns what the window's tree, canvas and popovers emitted into
//! graph edits and panel actions, and the card lane's dispatch. The live App
//! (`window_input.rs`, `app_render.rs`) and the ui-snap script runner call the
//! same functions.
//!
//! What differs is input presence only. The App reads winit events and a
//! window's size and scale; the runner synthesizes the same events at a fixed
//! logical size. The 3D viewport's gizmo needs the App's scene projection, so
//! a press or drag on it comes back to the caller as an outcome; the runner's
//! editor never opens a viewport session, so it never sees one.

use crossbeam_channel::Sender;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId};
use manifold_ui::cursors::TimelineCursor;
use manifold_ui::graph_canvas::mapping_popover::MappingPopover;
use manifold_ui::graph_canvas::{GraphCanvas, Rect as CanvasRect};
use manifold_ui::input::{Modifiers, PointerAction, UIEvent};
use manifold_ui::node::{NodeId, Vec2};
use manifold_ui::panels::PanelAction;
use manifold_ui::{GraphEditCommand, Rect as UiRect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta};

use crate::app::SelectionState;
use crate::content_command::ContentCommand;
use crate::text_input::TextInputState;
use crate::text_session::TextKey;
use crate::workspace::Workspace;

/// Everything one editor pointer event reads and writes. The caller borrows
/// it from its own state for the one event.
pub(crate) struct EditorPointer<'a> {
    pub ws: &'a mut Workspace,
    pub canvas: Option<&'a mut GraphCanvas>,
    /// The card lane's sideways mapping drawer.
    pub mapping: &'a mut MappingPopover,
    /// The window in logical pixels.
    pub area: UiRect,
    pub modifiers: Modifiers,
    pub time: f32,
    pub project: &'a Project,
    pub content_tx: Option<&'a Sender<ContentCommand>>,
    pub is_playing: bool,
    pub watched: Option<&'a GraphTarget>,
    /// The watched graph's renderer snapshot, for the canvas's right-click
    /// binding lookup.
    pub graph: Option<&'a manifold_node_engine::snapshot::GraphSnapshot>,
    pub active_layer: &'a mut Option<LayerId>,
    pub selection: &'a mut SelectionState,
}

impl EditorPointer<'_> {
    fn send(&self, cmd: ContentCommand) {
        if let Some(tx) = self.content_tx {
            ContentCommand::send(tx, cmd);
        }
    }

    /// The pointer position the canvas tracks; every button and wheel event
    /// reads it, since winit gives those no position.
    fn cursor(&self) -> Vec2 {
        let (x, y) = self.canvas.as_ref().map(|c| c.cursor()).unwrap_or((0.0, 0.0));
        Vec2::new(x, y)
    }

    /// The canvas viewport, the panel split and the card lane's left edge,
    /// from the same `Dock` the present pass reads, so column drags move the
    /// hit regions with the paint.
    fn columns(&self) -> (f32, f32, CanvasRect) {
        let r = self.ws.dock.rects(self.area);
        (r.left.width, r.right.x, CanvasRect::new(r.canvas.x, r.canvas.y, r.canvas.width, r.canvas.height))
    }
}

/// What a pointer move asks of the caller.
pub(crate) struct Moved {
    /// The cursor shape the dock wants.
    pub cursor: TimelineCursor,
    /// An armed viewport gizmo drag claimed the move: run it at this point.
    pub gizmo_move: Option<Vec2>,
}

/// `CursorMoved` in the editor window: dock drag or hover, mini-timeline
/// scrub, viewport drags, then the canvas and popovers, and the editor tree
/// when the cursor is over a side column, the node picker is open or a
/// widget holds the press.
pub(crate) fn cursor_moved(cx: &mut EditorPointer<'_>, pos: Vec2) -> Moved {
    // Column-divider drag or hover takes precedence over the canvas and
    // panels. A live drag resizes the dock and consumes the move. The dock's
    // sizes, drag and hover are its composite key (`Dock::draw_key`).
    let ws = &mut *cx.ws;
    let dock_dragging = if ws.dock.is_dragging() {
        ws.dock.drag(cx.area, pos);
        true
    } else {
        ws.dock.set_hover_from(cx.area, pos);
        false
    };
    let mut moved = Moved { cursor: ws.dock.cursor().unwrap_or(TimelineCursor::Default), gizmo_move: None };
    if dock_dragging {
        return moved;
    }

    // Mini-timeline scrub: a move while scrubbing seeks the playhead and
    // consumes the move. The playhead is part of the composite key, so the
    // seek draws when the content thread reports it.
    if ws.timeline_scrubbing {
        let bottom = ws.dock.rects(cx.area).bottom;
        let total = cx.project.timeline.duration_beats().as_f32();
        let beat = manifold_ui::MiniTimeline::beat_at_x(bottom, total, pos.x);
        cx.send(ContentCommand::SeekToBeat(manifold_core::Beats::from_f32(beat)));
        return moved;
    }

    // A gizmo axis drag claims the move outright; a press that armed one did
    // not arm the orbit drag below, so the two never share a gesture.
    if ws.viewport_gizmo_drag.is_some() {
        moved.gizmo_move = Some(pos);
        return moved;
    }

    // 3D viewport navigation: a drag a press inside the viewport started
    // claims every move until release, even outside the rect. The camera
    // shows only in the viewport's rendered image, a live surface the editor
    // composites every frame while it is on screen.
    if let Some((button, last_x, last_y)) = ws.viewport_drag {
        let (dx, dy) = (pos.x - last_x, pos.y - last_y);
        ws.viewport_drag = Some((button, pos.x, pos.y));
        if let Some(session) = ws.viewport_session.as_mut()
            && let Some(gesture) = crate::viewport_input::classify_mouse_drag(button, cx.modifiers.shift, dx, dy)
        {
            crate::viewport_input::apply(
                session.camera_mut(),
                gesture,
                &crate::viewport_input::ViewportInputSensitivity::default(),
            );
        }
        return moved;
    }

    let (preview_width, card_x, viewport) = cx.columns();
    // The canvas always tracks the cursor: graph-space coords need it even
    // for clicks that land in a side column. What the move changes reaches
    // the composite by its owner: the canvas's view version, the card
    // lane's drawer as a live surface while open, tree hover as dirty nodes.
    if let Some(canvas) = cx.canvas.as_deref_mut() {
        canvas.on_pointer_move(viewport, pos.x, pos.y);
        // The canvas popover's live range drag and handle hover.
        canvas.popover_on_move(pos.x, pos.y);
    }
    cx.mapping.on_move(pos.x, pos.y);
    // Canvas moves would only cause spurious hover and exit on tree nodes,
    // except over the node picker, which overlays the whole window.
    let in_panel = pos.x < preview_width || pos.x >= card_x;
    let ui_root = &mut cx.ws.ui_root;
    if in_panel || ui_root.browser_popup.is_open() || ui_root.input.pressed_widget().is_some() {
        ui_root.input.process_pointer(&mut ui_root.tree, pos, PointerAction::Move, cx.time);
    }
    moved
}

/// A graph-editor UI press stays owned by the editor tree after the cursor
/// crosses into the canvas: the hit region chooses the owner on press only, so
/// the tree keeps receiving the threshold move and the terminal release.
#[inline]
fn ui_pointer_owned(in_panel: bool, pressed_widget: bool) -> bool {
    in_panel || pressed_widget
}

/// What a button event asks of the caller.
#[derive(Default)]
pub(crate) struct Button {
    /// A left press landed in the 3D viewport: try a gizmo grab or object
    /// pick there, and when nothing takes it, [`arm_viewport_drag`].
    pub viewport_press: Option<Vec2>,
    /// A release ended this gizmo drag: commit its scrub.
    pub gizmo_release: Option<crate::scene_gizmo::GizmoGesture>,
}

/// A press in the 3D viewport that the gizmo did not take orbits (left) or
/// pans (middle) until release.
pub(crate) fn arm_viewport_drag(ws: &mut Workspace, button: MouseButton, pos: Vec2) {
    ws.viewport_drag = Some((button, pos.x, pos.y));
}

/// `MouseInput` in the editor window: the node picker is modal; then the dock
/// dividers, the mini-timeline, the 3D viewport, and finally the mapping
/// drawer, the canvas popover, the side columns and the canvas.
pub(crate) fn button(cx: &mut EditorPointer<'_>, button: MouseButton, state: ElementState) -> Button {
    let mut out = Button::default();
    let pos = cx.cursor();
    // The node picker claims every click in the window; its backdrop and
    // cells live in the editor tree.
    if cx.ws.ui_root.browser_popup.is_open() {
        if button == MouseButton::Left {
            let action = match state {
                ElementState::Pressed => PointerAction::Down,
                ElementState::Released => PointerAction::Up,
            };
            // The picker is overlay-tree content: a press or release dirties
            // its nodes, which the composite reads.
            let ui_root = &mut cx.ws.ui_root;
            ui_root.input.process_pointer(&mut ui_root.tree, pos, action, cx.time);
        }
        return out;
    }
    // Column dividers: a press on a handle begins the drag, a release ends it.
    if button == MouseButton::Left {
        let ws = &mut *cx.ws;
        match state {
            ElementState::Pressed => {
                if let Some(edge) = ws.dock.hit_test(cx.area, pos) {
                    ws.dock.begin(edge, pos);
                    return out;
                }
            }
            ElementState::Released => {
                if ws.dock.is_dragging() {
                    ws.dock.end();
                    return out;
                }
            }
        }
    }
    // Bottom mini-timeline: the play button toggles transport, a gutter click
    // picks that row's layer, a press in the strip body seeks and scrubs.
    // Each lands through an input the composite already reads: the play
    // state and playhead in its key, the layer pick through the inspector
    // projection that rebuilds the card lane.
    if button == MouseButton::Left && cx.ws.dock.show_bottom {
        let bottom = cx.ws.dock.rects(cx.area).bottom;
        match state {
            ElementState::Pressed => {
                if manifold_ui::MiniTimeline::hit_play(bottom, pos) {
                    cx.send(if cx.is_playing { ContentCommand::Pause } else { ContentCommand::Play });
                    return out;
                }
                // The same pick a main-timeline layer-header click makes, so
                // the editor's mirrored inspector follows it too.
                if let Some(row) = manifold_ui::MiniTimeline::row_at_y(bottom, cx.project.timeline.layers.len(), pos)
                    && let Some(layer) = cx.project.timeline.layers.get(row)
                {
                    let layer_id = layer.layer_id.clone();
                    *cx.active_layer = Some(layer_id.clone());
                    cx.selection.select_layer(layer_id);
                    return out;
                }
                if manifold_ui::MiniTimeline::body_rect(bottom).contains(pos) {
                    let total = cx.project.timeline.duration_beats().as_f32();
                    let beat = manifold_ui::MiniTimeline::beat_at_x(bottom, total, pos.x);
                    cx.send(ContentCommand::SeekToBeat(manifold_core::Beats::from_f32(beat)));
                    cx.ws.timeline_scrubbing = true;
                    return out;
                }
            }
            ElementState::Released => {
                if std::mem::take(&mut cx.ws.timeline_scrubbing) {
                    return out;
                }
            }
        }
    }
    // 3D viewport: a release always ends a gizmo or navigation drag first,
    // even outside the rect; a press inside it starts one. Both consume the
    // event, so the canvas never sees clicks the viewport claims. What they
    // change shows in the viewport's rendered image, a live surface.
    if state == ElementState::Released
        && button == MouseButton::Left
        && let Some(gesture) = cx.ws.viewport_gizmo_drag.take()
    {
        out.gizmo_release = Some(gesture);
        return out;
    }
    if state == ElementState::Released && cx.ws.viewport_drag.take().is_some() {
        return out;
    }
    if state == ElementState::Pressed
        && matches!(button, MouseButton::Left | MouseButton::Middle)
        && cx.ws.viewport_session.is_some()
        && cx.ws.viewport_rect.is_some_and(|r| r.contains(pos))
    {
        // Left tries a gizmo grab or object pick first; middle always pans.
        if button == MouseButton::Left {
            out.viewport_press = Some(pos);
        } else {
            arm_viewport_drag(cx.ws, button, pos);
        }
        return out;
    }
    let (preview_width, card_x, viewport) = cx.columns();
    let Some(canvas) = cx.canvas.as_deref_mut() else { return out };
    // Both side columns live in the editor tree; the canvas only sees clicks
    // in the centre.
    let in_panel = pos.x < preview_width || pos.x >= card_x;
    match (button, state) {
        (MouseButton::Left, ElementState::Pressed) => {
            // The mapping drawer floats over the canvas, so it gets first
            // crack: consumed, done; open but missed, close it and fall through.
            let mut consumed = false;
            if cx.mapping.is_open() {
                if cx.mapping.on_press(pos.x, pos.y) {
                    consumed = true;
                } else {
                    cx.mapping.close();
                }
            }
            if consumed || (canvas.popover_open() && canvas.popover_on_left_press(pos.x, pos.y)) {
                // A popover took it.
            } else if in_panel {
                // A card click retargets the canvas through the card-clicked
                // dispatch; sliders, tabs and drags resolve in the inspector.
                let ui_root = &mut cx.ws.ui_root;
                ui_root.input.process_pointer(&mut ui_root.tree, pos, PointerAction::Down, cx.time);
            } else {
                canvas.on_left_button_down(viewport, pos.x, pos.y, cx.time, cx.modifiers.shift);
            }
        }
        (MouseButton::Left, ElementState::Released) => {
            // Commit any in-progress drawer or popover drag.
            cx.mapping.on_release();
            canvas.popover_on_left_release();
            let ui_root = &mut cx.ws.ui_root;
            if ui_pointer_owned(in_panel, ui_root.input.pressed_widget().is_some()) {
                ui_root.input.process_pointer(&mut ui_root.tree, pos, PointerAction::Up, cx.time);
            }
            // The canvas owns its captured drag and its release is idempotent,
            // so it always gets the terminal event.
            canvas.on_left_button_up(viewport, pos.x, pos.y);
        }
        (MouseButton::Right, ElementState::Pressed) => {
            // Right-click an exposed param row on a node: the canvas resolves
            // the row and anchor, the project resolves the binding.
            if !in_panel
                && let Some((node_id, pi)) = canvas.on_right_button_down(viewport, pos.x, pos.y)
                && let Some(inner_param) = canvas.param_name_at(node_id, pi)
                && let Some((binding_id, label, min, max, invert, curve, scale, offset, range, is_angle, section)) =
                    crate::app_render::resolve_canvas_binding(cx.graph, cx.watched, cx.project, node_id, &inner_param)
            {
                canvas.open_mapping_popover(
                    crate::editing_host::to_ui_graph_target(cx.watched.expect("resolved binding has a graph target")),
                    viewport, node_id, pi, binding_id, label, min, max, invert,
                    crate::ui_translate::macro_curve_to_ui(curve), scale, offset, range,
                    is_angle, section,
                );
            }
        }
        (MouseButton::Middle, ElementState::Pressed) => {
            if !in_panel {
                canvas.on_pan_button_down(pos.x, pos.y);
            }
        }
        (MouseButton::Middle, ElementState::Released) => {
            // Pan capture survives crossing into a side column.
            canvas.on_pan_button_up();
        }
        _ => {}
    }
    out
}

/// `MouseWheel` in the editor window: dolly over the 3D viewport, scroll the
/// card lane, else zoom the canvas at the cursor. False when there is no
/// canvas to zoom.
pub(crate) fn wheel(cx: &mut EditorPointer<'_>, delta: MouseScrollDelta) -> bool {
    let pos = cx.cursor();
    let ws = &mut *cx.ws;
    if let Some(rect) = ws.viewport_rect
        && rect.contains(pos)
        && let Some(session) = ws.viewport_session.as_mut()
    {
        // A wheel notch dollies. On macOS a trackpad pinch arrives as a
        // Ctrl-modified pixel scroll, which dollies too; a bare pixel scroll
        // is a two-finger pan.
        let gesture = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                crate::viewport_input::classify_scroll(y * crate::window_input::LINE_DELTA_PX)
            }
            MouseScrollDelta::PixelDelta(p) if cx.modifiers.ctrl => {
                crate::viewport_input::classify_trackpad_pinch_dolly(p.y as f32)
            }
            MouseScrollDelta::PixelDelta(p) => crate::viewport_input::classify_trackpad_pan(p.x as f32, p.y as f32),
        };
        crate::viewport_input::apply(
            session.camera_mut(),
            gesture,
            &crate::viewport_input::ViewportInputSensitivity::default(),
        );
        return true;
    }
    // The card lane scrolls like the main window's inspector: in place, its
    // moved nodes dirty, the next editor frame's plan repaints the lane.
    if pos.x >= ws.dock.rects(cx.area).right.x {
        let (_, dy) = crate::window_input::normalize_scroll_delta(delta);
        if !ws.ui_root.try_inspector_scroll(dy, pos.x) {
            ws.ui_root.inspector.handle_scroll_at(dy, pos.x);
        }
        return true;
    }
    let Some(canvas) = cx.canvas.as_deref_mut() else { return false };
    let (_, dy) = crate::window_input::normalize_scroll_delta(delta);
    canvas.on_scroll(CanvasRect::new(0.0, 0.0, cx.area.width, cx.area.height), dy);
    true
}

/// A pinch zooms the canvas at the cursor; the 3D viewport and the card lane
/// ignore it.
pub(crate) fn pinch(cx: &mut EditorPointer<'_>, magnification: f32) {
    let Some(canvas) = cx.canvas.as_deref_mut() else { return };
    let (x, y) = canvas.cursor();
    let over_viewport = cx.ws.viewport_rect.is_some_and(|rect| rect.contains(Vec2::new(x, y)));
    if over_viewport || x >= cx.ws.dock.rects(cx.area).right.x {
        return;
    }
    canvas.on_pinch(CanvasRect::new(0.0, 0.0, cx.area.width, cx.area.height), magnification);
}

/// What cancelling the editor's pointer capture asks of the caller.
pub(crate) struct Cancelled {
    /// Something was cancelled.
    pub any: bool,
    /// A gizmo drag ended through its release: commit its scrub.
    pub gizmo_release: Option<crate::scene_gizmo::GizmoGesture>,
}

/// Ends every editor-owned pointer gesture when the OS withholds the release
/// (focus loss) or Escape cancels it.
///
/// The editor tree gets a terminal `Up` even when the cursor is outside its
/// panels: a press that never crossed the drag threshold is sent off-screen so
/// cancelling cannot become a click; an active drag ends at the real cursor
/// through its normal end path. Canvas wire, marquee and pan captures are
/// discarded first, so a stale cursor over a port cannot connect.
pub(crate) fn cancel_capture(cx: &mut EditorPointer<'_>) -> Cancelled {
    let ui_pressed = cx.ws.ui_root.input.pressed_widget().is_some();
    let ui_dragging = cx.ws.ui_root.input.is_dragging();
    let cursor = cx.cursor();
    let had_mapping = cx.mapping.is_open();
    let had_canvas_popover = cx.canvas.as_ref().is_some_and(|canvas| canvas.popover_open());
    let (_, _, viewport) = cx.columns();
    let canvas_capture = cx.canvas.as_deref_mut().is_some_and(|canvas| canvas.finish_pointer_capture(viewport));
    // Cancel the mapping surfaces before the release path, whose `on_release`
    // would otherwise commit a live scrub.
    cx.mapping.close();
    if let Some(canvas) = cx.canvas.as_deref_mut()
        && canvas.popover_open()
    {
        canvas.close_mapping_popover();
    }
    if ui_pressed {
        let terminal = if ui_dragging { cursor } else { Vec2::new(-1.0, -1.0) };
        let ui_root = &mut cx.ws.ui_root;
        ui_root.input.process_pointer(&mut ui_root.tree, terminal, PointerAction::Up, cx.time);
    }
    // The single terminal paths for canvas and viewport gestures; both are
    // idempotent when nothing is active.
    let gizmo_release = button(cx, MouseButton::Left, ElementState::Released).gizmo_release;
    button(cx, MouseButton::Middle, ElementState::Released);
    Cancelled {
        any: canvas_capture || ui_pressed || had_mapping || had_canvas_popover || gizmo_release.is_some(),
        gizmo_release,
    }
}

/// A key while a mapping popover field is being typed into: the card lane's
/// drawer first, then the canvas popover. Digits, `.`, `-` and Backspace edit,
/// Enter commits, Escape cancels, ahead of every canvas shortcut (Backspace
/// would otherwise delete the selected node). Command and Ctrl chords pass
/// through untyped. False when neither popover is typing. The card lane's
/// drawer is a live surface of the composite while open; the canvas popover
/// moves the canvas's view version.
pub(crate) fn popover_key(
    mapping: &mut MappingPopover,
    canvas: Option<&mut GraphCanvas>,
    key: TextKey<'_>,
    modifiers: Modifiers,
) -> bool {
    let typing = !modifiers.command && !modifiers.ctrl;
    if mapping.is_editing() {
        match key {
            TextKey::Enter => mapping.commit_edit(),
            TextKey::Escape => mapping.cancel_edit(),
            TextKey::Backspace => mapping.on_backspace(),
            TextKey::Space if typing => mapping.on_text_char(' '),
            TextKey::Chars(text) if typing => text.chars().for_each(|ch| mapping.on_text_char(ch)),
            _ => {}
        }
    } else if let Some(canvas) = canvas
        && canvas.popover_is_editing()
    {
        match key {
            TextKey::Enter => canvas.popover_commit_edit(),
            TextKey::Escape => canvas.popover_cancel_edit(),
            TextKey::Backspace => canvas.popover_on_backspace(),
            TextKey::Space if typing => canvas.popover_on_text_char(' '),
            TextKey::Chars(text) if typing => text.chars().for_each(|ch| canvas.popover_on_text_char(ch)),
            _ => {}
        }
    } else {
        return false;
    }
    true
}

/// The left preview column's "Smart preview" toggle: its button id from the
/// last tree build and the value a click sets.
pub(crate) struct SmartPreviewToggle<'a> {
    pub intents: &'a mut manifold_ui::intent::IntentRegistry<GraphEditCommand>,
    pub id: Option<NodeId>,
    pub normalize: bool,
}

/// What the editor window emitted since the last frame.
#[derive(Default)]
pub(crate) struct EditorDrain {
    /// Canvas and sidebar edits, in their own vocabulary.
    pub graph_edits: Vec<GraphEditCommand>,
    /// The card lane's actions; they dispatch against the editor's own tree.
    pub card_actions: Vec<PanelAction>,
    /// The popovers' mapping actions; they carry their own owner and join the
    /// main window's queue.
    pub actions: Vec<PanelAction>,
}

/// Drains the editor window's tree events, its canvas and both popovers. The
/// node picker is modal and takes every click; otherwise the smart-preview
/// toggle resolves first and the rest goes through the inspector's own event
/// routing, as in the main window. Overlay-hosted text sessions follow their
/// overlay.
pub(crate) fn drain(
    ws: Option<&mut Workspace>,
    canvas: Option<&mut GraphCanvas>,
    mapping: &mut MappingPopover,
    text_input: &mut TextInputState,
    toggle: SmartPreviewToggle<'_>,
) -> EditorDrain {
    use crate::text_input::{AnchorRect, TextInputField, TextSessionOwner};
    use crate::ui_root::OverlayId;
    let mut out = EditorDrain::default();
    if let Some(ed) = ws {
        let events = ed.ui_root.input.drain_events();
        if ed.ui_root.browser_popup.is_open() {
            use manifold_ui::panels::browser_popup::BrowserPopupAction;
            for event in events {
                let UIEvent::Click { node_id, .. } = event else { continue };
                if ed.ui_root.browser_popup.is_search_bar(node_id) {
                    // Already focused on open; a click re-focuses.
                    let r = ed.ui_root.browser_popup.search_bar_rect(&ed.ui_root.tree);
                    text_input.begin_owned(
                        TextSessionOwner::EditorOverlay(OverlayId::BrowserPopup),
                        TextInputField::SearchFilter,
                        ed.ui_root.browser_popup.current_filter(),
                        AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    continue;
                }
                // `handle_click` bypasses the overlay driver's closed-overlay
                // tracking, so record a close here; the pump below cancels
                // the popup's text session. A close moves the overlays'
                // open set, which the editor's plan rebuilds.
                let was_open = ed.ui_root.browser_popup.is_open();
                if let Some(action) = ed.ui_root.browser_popup.handle_click(node_id) {
                    ed.ui_root.note_overlay_closed_if(OverlayId::BrowserPopup, was_open);
                    // `graph_pos` is the canvas position captured at open,
                    // passed straight through.
                    if let BrowserPopupAction::NodeSelected { type_id, graph_pos } = action {
                        out.graph_edits.push(GraphEditCommand::AddGraphNodeAt { type_id, graph_pos });
                    }
                } else if ed.ui_root.browser_popup.contains_node(&ed.ui_root.tree, node_id) {
                    ed.ui_root.note_overlay_closed_if(OverlayId::BrowserPopup, was_open);
                    // An internal click (category chip, background) does not
                    // leak to the canvas. A chip changes what the popup
                    // lists, which shows only after the overlays rebuild.
                    ed.ui_root.mark_overlay_dirty();
                }
            }
        } else {
            if !events.is_empty() {
                toggle.intents.clear();
                if let Some(id) = toggle.id {
                    toggle.intents.on(
                        id,
                        manifold_ui::intent::Gesture::Click,
                        GraphEditCommand::SetNodePreviewNormalize(!toggle.normalize),
                    );
                }
            }
            for event in &events {
                if let UIEvent::Click { node_id, .. } = event
                    && let Some(cmd) =
                        toggle.intents.resolve(&ed.ui_root.tree, Some(*node_id), manifold_ui::intent::Gesture::Click)
                {
                    out.graph_edits.push(cmd);
                }
            }
            out.card_actions.extend(ed.ui_root.route_inspector_events(&events));
        }
        // The node picker's search session follows the popup's real search
        // bar; it opened anchored at zero, before the popup's tree existed.
        if text_input.owner == Some(TextSessionOwner::EditorOverlay(OverlayId::BrowserPopup))
            && text_input.field == TextInputField::SearchFilter
        {
            let r = ed.ui_root.browser_popup.search_bar_rect(&ed.ui_root.tree);
            if r.width > 0.0 && r.height > 0.0 {
                text_input.update_anchor(AnchorRect::new(r.x, r.y, r.width, r.height));
            }
        }
        for id in ed.ui_root.take_closed_overlays() {
            text_input.cancel_if_owned_by(TextSessionOwner::EditorOverlay(id));
        }
    }
    // The canvas owns its pointer state, so its edits bypass the tree.
    if let Some(canvas) = canvas {
        out.graph_edits.extend(canvas.drain_edits());
        out.actions.extend(canvas.drain_popover_actions());
    }
    out.actions.extend(mapping.drain_actions());
    out
}


/// Opens the text session a card-lane action asks for, against the editor's
/// own inspector tab, and returns the mapping drawers the lane asked for. The
/// caller opens those (it watches the drawer's graph first) before
/// [`dispatch_card_lane`].
pub(crate) fn card_lane_entries<'x>(
    text_input: &mut TextInputState,
    ws: &mut Workspace,
    project: &Project,
    selection: &SelectionState,
    active_layer: &Option<LayerId>,
    actions: &'x [PanelAction],
) -> Vec<(&'x manifold_ui::GraphParamTarget, &'x str, NodeId)> {
    let mut mappings = Vec::new();
    for action in actions {
        let tab = ws.ui_root.inspector.last_effect_tab();
        // The opened field is a live surface of the editor's composite.
        if crate::text_session::begin_inspector_entry(text_input, action, true, tab, active_layer, selection, project) {
            continue;
        }
        if let PanelAction::Root(manifold_ui::RootAction::OpenCardMapping { target, param_id, anchor_node_id }) = action {
            mappings.push((target, param_id.as_ref(), *anchor_node_id));
        }
    }
    mappings
}

/// What the card lane's dispatch reads and writes besides the editor window.
pub(crate) struct CardLane<'a> {
    pub ws: &'a mut Workspace,
    pub project: crate::ui_bridge::ProjectRef<'a>,
    pub content_tx: &'a Sender<ContentCommand>,
    pub content_state: &'a crate::content_state::ContentState,
    pub selection: &'a mut SelectionState,
    pub active_layer: &'a mut Option<LayerId>,
    pub user_prefs: &'a mut crate::user_prefs::UserPrefs,
    pub scrub: &'a mut crate::ui_bridge::ScrubState,
    pub dialogs: crate::file_dialogs::DialogHost<'a>,
}

/// What the card lane's actions ask of the caller.
#[derive(Default)]
pub(crate) struct CardLaneOutcome {
    /// A card click retargets the canvas to that effect's graph (row index in
    /// the editor's inspector tab).
    pub retarget_effect: Option<usize>,
    /// A generator card click retargets the canvas to the generator's graph.
    pub retarget_generator: bool,
    pub save_preset: Option<(
        manifold_core::preset_def::PresetKind,
        manifold_core::effect_graph_def::EffectGraphDef,
        crate::text_input::SavePresetDestination,
    )>,
}

/// Dispatches the card lane's actions against the editor's own tree, as the
/// main window's inspector actions dispatch against the main tree: the editor
/// mirrors the main window's selection and active layer, so param edits
/// resolve identically, and only the editor's transient visuals (collapse,
/// highlight, card drag) land on its tree.
pub(crate) fn dispatch_card_lane(mut lane: CardLane<'_>, actions: &[PanelAction]) -> CardLaneOutcome {
    use manifold_ui::panels::ParamsAction;
    let mut out = CardLaneOutcome::default();
    for action in actions {
        match action {
            PanelAction::Params(ParamsAction::EffectCardClicked(ei)) => out.retarget_effect = Some(*ei),
            PanelAction::Params(ParamsAction::GenCardClicked) => out.retarget_generator = true,
            _ => {}
        }
        let mut dctx = crate::ui_bridge::DispatchCtx {
            project: lane.project.reborrow(),
            content_tx: lane.content_tx,
            content_state: lane.content_state,
            ui: &mut lane.ws.ui_root,
            selection: &mut *lane.selection,
            active_layer: &mut *lane.active_layer,
            user_prefs: &mut *lane.user_prefs,
            editor_target: None,
            scrub: &mut *lane.scrub,
            dialogs: lane.dialogs,
        };
        let result = crate::ui_bridge::dispatch(action, &mut dctx);
        if result.begin_save_preset.is_some() {
            out.save_preset = result.begin_save_preset;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_ui_capture_survives_crossing_out_of_panel() {
        assert!(ui_pointer_owned(false, true));
        assert!(ui_pointer_owned(true, false));
        assert!(!ui_pointer_owned(false, false));
    }
}
