//! Opt-in live UI control. Observations come from the built primary window;
//! gestures enter the same `input_*` functions as winit events. No project writes.
mod transport;

use std::collections::VecDeque;
use std::path::Path;

use manifold_ui::automation::{self, AutomationAction, AutomationTarget, Gesture};
use manifold_ui::clip_hit_tester::ClipHitTargets;
use manifold_ui::input::Modifiers;
use manifold_ui::{Rect, UIFlags, Vec2};
use serde::Deserialize;
use serde_json::{Value, json};
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::{Key, NamedKey};

use crate::app::Application;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Observe {
        #[serde(default)]
        contains: Option<String>,
    },
    Resolve {
        target: AutomationTarget,
    },
    Act {
        action: AutomationAction,
    },
    TimelinePoint {
        beat: f32,
        layer: usize,
    },
}

enum Input {
    Move(Vec2),
    Button(MouseButton, ElementState),
    Wheel(Vec2),
    Key(Key),
    Modifiers(Modifiers),
    Wait,
}

struct Pending {
    id: Value,
    generation: u64,
    events: VecDeque<Input>,
    original_modifiers: Modifiers,
    button: Option<MouseButton>,
}

pub(crate) struct LiveUi {
    transport: transport::Transport,
    frame: u64,
    pending: Option<Pending>,
}

impl LiveUi {
    pub(crate) fn bind(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            transport: transport::Transport::bind(path)?,
            frame: 0,
            pending: None,
        })
    }
}

impl Application {
    pub(crate) fn tick_live_ui(&mut self) {
        let Some(mut live) = self.live_ui.take() else {
            return;
        };
        live.frame += 1;
        let incoming = live.transport.poll();
        if let Some(mut pending) = live.pending.take() {
            if !live.transport.connected() || live.transport.generation() != pending.generation {
                // A disconnected client must never leave a held button/modifier.
                self.release_live_input(&pending);
            } else if let Some(event) = pending.events.pop_front() {
                if let Input::Button(button, state) = &event {
                    pending.button = (*state == ElementState::Pressed).then_some(*button);
                }
                self.apply_live_input(event);
                live.pending = Some(pending);
            } else {
                self.release_live_input(&pending);
                live.transport.reply(json!({"id": pending.id, "ok": true,
                    "frame": live.frame, "dispatched": true, "state": self.live_summary()}));
            }
        }
        if let Some(mut value) = incoming {
            let id = value
                .as_object_mut()
                .and_then(|o| o.remove("id"))
                .unwrap_or(Value::Null);
            let result = if live.pending.is_some() {
                Err("an input sequence is already active".to_owned())
            } else {
                serde_json::from_value::<Request>(value)
                    .map_err(|e| e.to_string())
                    .and_then(|request| self.handle_live_request(request, &mut live, id.clone()))
            };
            match result {
                Ok(Some(data)) => live
                    .transport
                    .reply(json!({"id": id, "ok": true, "frame": live.frame, "data": data})),
                Ok(None) => {}
                Err(error) => live
                    .transport
                    .reply(json!({"id": id, "ok": false, "frame": live.frame, "error": error})),
            }
        }
        self.live_ui = Some(live);
    }

    fn handle_live_request(
        &mut self,
        request: Request,
        live: &mut LiveUi,
        id: Value,
    ) -> Result<Option<Value>, String> {
        match request {
            Request::Observe { contains } => Ok(Some(self.live_observation(contains.as_deref()))),
            Request::Resolve { target } => {
                let rect = self.live_resolve(&target)?;
                Ok(Some(json!({"rect": rect, "state": self.live_summary()})))
            }
            Request::TimelinePoint { beat, layer } => {
                let viewport = &self.ws.ui_root.viewport;
                if !beat.is_finite() || beat < 0.0 || layer >= viewport.layer_count() {
                    return Err("invalid beat or layer".into());
                }
                let mapper = viewport.mapper();
                let tracks = viewport.tracks_rect();
                let point = Vec2::new(
                    tracks.x + mapper.beat_to_pixel(manifold_core::Beats::from_f32(beat)),
                    tracks.y + mapper.get_layer_y_offset(layer) - viewport.scroll_y_px()
                        + mapper.get_layer_height(layer) * 0.5,
                );
                if !tracks.contains(point) || mapper.get_layer_height(layer) <= 0.0 {
                    return Err(
                        "timeline position is offscreen; zoom or scroll using the UI first".into(),
                    );
                }
                Ok(Some(json!({"point": point})))
            }
            Request::Act { action } => {
                let events = self.live_events(action)?;
                live.pending = Some(Pending {
                    id,
                    generation: live.transport.generation(),
                    events,
                    original_modifiers: self.modifiers,
                    button: None,
                });
                Ok(None)
            }
        }
    }

    fn live_summary(&self) -> Value {
        let layers: Vec<_> = self
            .local_project
            .timeline
            .layers
            .iter()
            .map(|layer| {
                let clips: Vec<_> = layer
                    .clips
                    .iter()
                    .map(|clip| {
                        json!({"id": clip.id,
                "startBeat": clip.start_beat, "durationBeats": clip.duration_beats})
                    })
                    .collect();
                json!({"id": layer.layer_id, "name": layer.name, "type": layer.layer_type,
                "generator": layer.generator_type(), "clips": clips})
            })
            .collect();
        json!({"pid": std::process::id(), "window": "primary", "dataVersion": self.content_state.data_version,
            "playing": self.content_state.is_playing, "beat": self.content_state.current_beat,
            "bpm": self.content_state.bpm, "timeSignature": [self.local_project.settings.time_signature_numerator,
                self.local_project.settings.time_signature_denominator], "layers": layers})
    }

    fn live_observation(&self, contains: Option<&str>) -> Value {
        let tree = &self.ws.ui_root.tree;
        let filter = contains.map(str::to_lowercase);
        let nodes: Vec<_> = tree
            .nodes()
            .iter()
            .filter(|n| {
                let useful = tree.name_of(n.id).is_some()
                    || n.text.is_some()
                    || n.flags.contains(UIFlags::INTERACTIVE);
                useful
                    && n.flags.contains(UIFlags::VISIBLE)
                    && filter.as_ref().is_none_or(|f| {
                        tree.name_of(n.id).unwrap_or("").to_lowercase().contains(f)
                            || n.text.as_deref().unwrap_or("").to_lowercase().contains(f)
                    })
            })
            .map(|n| {
                json!({"widget": format!("{:016x}", tree.widget_of(n.id).raw()),
            "name": tree.name_of(n.id), "text": n.text, "type": format!("{:?}", n.node_type),
            "rect": n.bounds, "interactive": n.flags.contains(UIFlags::INTERACTIVE),
            "disabled": n.flags.contains(UIFlags::DISABLED)})
            })
            .collect();
        let mut clips = Vec::new();
        self.ws.ui_root.viewport.visible_clip_rects(&mut clips);
        let clips: Vec<_> = clips
            .iter()
            .map(|c| {
                json!({"id": c.clip_id, "name": c.name, "rect": c.rect,
            "startBeat": c.start_beat, "endBeat": c.end_beat})
            })
            .collect();
        json!({"protocol": 1, "coordinates": "window logical pixels", "state": self.live_summary(),
            "nodes": nodes, "clips": clips, "tracks": self.ws.ui_root.viewport.tracks_rect()})
    }

    fn live_resolve(&self, target: &AutomationTarget) -> Result<Rect, String> {
        let mut clips = Vec::new();
        self.ws.ui_root.viewport.visible_clip_rects(&mut clips);
        let surface = ClipHitTargets(&clips);
        let tree = &self.ws.ui_root.tree;
        let resolved =
            automation::resolve(tree, &[&surface], target).map_err(|e| format!("{e:?}"))?;
        let rect = resolved.rect;
        let centre = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
        self.validate_live_point(centre)?;
        if let Some(node) = resolved.node {
            validate_widget(tree, node, centre)?;
        }
        Ok(rect)
    }

    fn validate_live_point(&self, point: Vec2) -> Result<(), String> {
        let id = self
            .primary_window_id
            .ok_or("primary window is unavailable")?;
        let window = &self
            .window_registry
            .get(&id)
            .ok_or("primary window is unavailable")?
            .window;
        let size = window.inner_size().to_logical::<f64>(window.scale_factor());
        if !point.x.is_finite()
            || !point.y.is_finite()
            || point.x < 0.0
            || point.y < 0.0
            || point.x as f64 >= size.width
            || point.y as f64 >= size.height
        {
            return Err("target is outside the visible window".into());
        }
        Ok(())
    }

    fn live_events(&self, action: AutomationAction) -> Result<VecDeque<Input>, String> {
        let mut out = VecDeque::new();
        match action {
            AutomationAction::Pointer { target, gesture } => {
                let rect = self.live_resolve(&target)?;
                let from = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
                out.push_back(Input::Modifiers(Modifiers::NONE));
                out.push_back(Input::Move(from));
                match gesture {
                    Gesture::Click { modifiers } => {
                        out.push_back(Input::Modifiers(modifiers));
                        click_events(&mut out, MouseButton::Left);
                    }
                    Gesture::DoubleClick => {
                        click_events(&mut out, MouseButton::Left);
                        click_events(&mut out, MouseButton::Left);
                    }
                    Gesture::RightClick => click_events(&mut out, MouseButton::Right),
                    Gesture::Hover => {},
                    Gesture::Scroll { delta } => {
                        if !delta.x.is_finite() || !delta.y.is_finite() || delta.x.abs().max(delta.y.abs()) > 10000.0 {
                            return Err("invalid scroll delta".into());
                        }
                        out.push_back(Input::Wheel(delta));
                    }
                    Gesture::Drag { to, steps } => {
                        if !(2..=60).contains(&steps) { return Err("drag steps must be 2..60".into()); }
                        let rect = self.live_resolve(&to)?;
                        let to = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
                        out.push_back(Input::Button(MouseButton::Left, ElementState::Pressed));
                        out.extend(automation::interpolate_drag(from, to, steps).into_iter().map(Input::Move));
                        out.push_back(Input::Move(to));
                        out.push_back(Input::Button(MouseButton::Left, ElementState::Released));
                    }
                }
            }
            AutomationAction::Key { key, modifiers } => {
                out.push_back(Input::Modifiers(modifiers));
                out.push_back(Input::Key(live_key(key)?));
            }
            AutomationAction::Text { text } => {
                if text.chars().count() > 120 { return Err("text is limited to 120 characters per request".into()); }
                out.push_back(Input::Modifiers(Modifiers::NONE));
                for c in text.chars() {
                    if c.is_control() { return Err("use a Key action for control characters".into()); }
                    out.push_back(Input::Key(Key::Character(c.to_string().into())));
                }
            }
            AutomationAction::Step { frames } => {
                if frames > 120 { return Err("wait is limited to 120 frames".into()); }
                out.extend((0..frames).map(|_| Input::Wait));
            }
            _ => return Err("unsupported live action; use Observe/Resolve and normal Pointer/Key/Text/Step input".into()),
        }
        // Dispatch acknowledgement is deliberately distinct from a state assertion.
        // Allow real render/content snapshots to catch up before returning state.
        out.extend((0..3).map(|_| Input::Wait));
        Ok(out)
    }

    fn apply_live_input(&mut self, event: Input) {
        let Some(id) = self.primary_window_id else {
            return;
        };
        match event {
            Input::Move(point) => {
                let scale = self
                    .window_registry
                    .get(&id)
                    .map_or(1.0, |w| w.window.scale_factor());
                self.input_cursor_moved(
                    id,
                    true,
                    false,
                    winit::dpi::PhysicalPosition::new(
                        point.x as f64 * scale,
                        point.y as f64 * scale,
                    ),
                );
            }
            Input::Button(button, state) => self.input_mouse_input(id, true, false, button, state),
            Input::Wheel(delta) => self.input_mouse_wheel(
                id,
                true,
                false,
                MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                    delta.x as f64,
                    delta.y as f64,
                )),
            ),
            Input::Key(key) => self.input_keyboard(true, false, key),
            Input::Modifiers(modifiers) => self.input_modifiers(modifiers),
            Input::Wait => {}
        }
    }

    fn release_live_input(&mut self, pending: &Pending) {
        if let Some(button) = pending.button {
            // Escape uses the normal cancellation path before releasing a drag.
            self.apply_live_input(Input::Key(Key::Named(NamedKey::Escape)));
            self.apply_live_input(Input::Button(button, ElementState::Released));
        }
        self.input_modifiers(pending.original_modifiers);
    }

    pub(crate) fn interrupt_live_ui(&mut self) {
        let Some(mut live) = self.live_ui.take() else {
            return;
        };
        if let Some(pending) = live.pending.take() {
            self.release_live_input(&pending);
            live.transport.reply(json!({"ok":false, "id":pending.id,
                "error":"native input interrupted the sequence; inspect state before retrying"}));
        }
        self.live_ui = Some(live);
    }
}

fn validate_widget(
    tree: &manifold_ui::UITree,
    node: manifold_ui::NodeId,
    point: Vec2,
) -> Result<(), String> {
    let mut ancestor = Some(node);
    while let Some(id) = ancestor {
        let n = &tree.nodes()[id.index()];
        if !n.flags.contains(UIFlags::VISIBLE)
            || n.flags.contains(UIFlags::DISABLED)
            || (n.flags.contains(UIFlags::CLIPS_CHILDREN) && !n.bounds.contains(point))
        {
            return Err(
                "target is hidden, disabled, or clipped; reveal it through the UI first".into(),
            );
        }
        ancestor = tree.parent_of(id);
    }
    let hit = tree
        .hit_test(point)
        .ok_or("target has no active hit area")?;
    let is_ancestor = |child, wanted| {
        let mut next = Some(child);
        while let Some(id) = next {
            if id == wanted {
                return true;
            }
            next = tree.parent_of(id);
        }
        false
    };
    if !is_ancestor(hit, node) && !is_ancestor(node, hit) {
        return Err("target is covered by another control; dismiss its overlay first".into());
    }
    Ok(())
}

fn click_events(out: &mut VecDeque<Input>, button: MouseButton) {
    out.push_back(Input::Button(button, ElementState::Pressed));
    out.push_back(Input::Button(button, ElementState::Released));
}

fn live_key(key: manifold_ui::input::Key) -> Result<Key, String> {
    use manifold_ui::input::Key as K;
    let named = match key {
        K::Space => NamedKey::Space,
        K::Enter => NamedKey::Enter,
        K::Escape => NamedKey::Escape,
        K::Backspace => NamedKey::Backspace,
        K::Delete => NamedKey::Delete,
        K::Tab => NamedKey::Tab,
        K::Left => NamedKey::ArrowLeft,
        K::Right => NamedKey::ArrowRight,
        K::Up => NamedKey::ArrowUp,
        K::Down => NamedKey::ArrowDown,
        K::Home => NamedKey::Home,
        K::End => NamedKey::End,
        K::PageUp => NamedKey::PageUp,
        K::PageDown => NamedKey::PageDown,
        _ => {
            let name = format!("{key:?}");
            if name.len() == 1 {
                return Ok(Key::Character(name.to_lowercase().into()));
            }
            if let Some(digit) = name.strip_prefix("Num") {
                return Ok(Key::Character(digit.into()));
            }
            return Err(format!("unsupported live key {name}"));
        }
    };
    Ok(Key::Named(named))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_rejects_unknown_fields_and_operations() {
        assert!(serde_json::from_value::<Request>(json!({"op":"observe","write":true})).is_err());
        assert!(serde_json::from_value::<Request>(json!({"op":"execute","code":"bad"})).is_err());
    }
    #[test]
    fn key_mapping_uses_native_logical_keys() {
        assert_eq!(
            live_key(manifold_ui::input::Key::Backspace).unwrap(),
            Key::Named(NamedKey::Backspace)
        );
        assert_eq!(
            live_key(manifold_ui::input::Key::Z).unwrap(),
            Key::Character("z".into())
        );
    }
    #[test]
    fn live_target_rejects_hidden_and_occluded_controls() {
        let mut tree = manifold_ui::UITree::new();
        let region = tree.begin_region(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            manifold_ui::tree::ZTier::Base,
            "test",
            UIFlags::empty(),
        );
        let start = tree.count();
        let node = tree.add_button(
            Some(region.root),
            0.0,
            0.0,
            40.0,
            20.0,
            manifold_ui::UIStyle::default(),
            "Play",
        );
        assert!(validate_widget(&tree, node, Vec2::new(10.0, 10.0)).is_ok());
        tree.set_visible(node, false);
        assert!(validate_widget(&tree, node, Vec2::new(10.0, 10.0)).is_err());
        tree.set_visible(node, true);
        tree.add_button(
            Some(region.root),
            0.0,
            0.0,
            40.0,
            20.0,
            manifold_ui::UIStyle::default(),
            "Overlay",
        );
        assert!(validate_widget(&tree, node, Vec2::new(10.0, 10.0)).is_err());
        tree.end_region(region, start);
    }
}
